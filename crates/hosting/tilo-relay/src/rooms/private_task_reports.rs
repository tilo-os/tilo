//! Exact automatic reports remain governor-private, never CRDT history.
use super::GovernedRoom;
use std::collections::{BTreeMap, BTreeSet};
use tilo_actions::task_messaging::{TaskReport, TaskReportKind, TaskReportRole, TaskRunAddress};
use tilo_peer_state::workspace_task::{
    self, WorkspaceTaskAuthority as A, WorkspaceTaskCommand as C,
};

// Write-ahead private custody. Pending entries are hidden until the matching
// device-scoped canonical receipt is durable. A crash between the two files
// therefore loses neither the prose nor the exact request binding on retry.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedReport {
    request_digest: [u8; 32],
    report: TaskReport,
}

fn report_key(device: [u8; 32], invocation: uuid::Uuid) -> String {
    format!("{}:{invocation}", hex::encode(device))
}

struct ReportContent<'a> {
    task_id: &'a String,
    epoch: u64,
    body: &'a String,
    kind: TaskReportKind,
    neutral: &'static str,
}

fn report_content(command: &C) -> Option<ReportContent<'_>> {
    let (task_id, epoch, body, kind, neutral) = match command {
        C::PublicProgress {
            task_id,
            run_epoch,
            message,
            ..
        } => (
            task_id,
            *run_epoch,
            message,
            TaskReportKind::Progress,
            "progress",
        ),
        C::Finish {
            task_id,
            run_epoch,
            summary,
            outcome,
        } => (
            task_id,
            *run_epoch,
            summary,
            TaskReportKind::Finish,
            match outcome {
                workspace_task::WorkspaceTaskFinish::Done => "done",
                workspace_task::WorkspaceTaskFinish::Waiting => "waiting",
                workspace_task::WorkspaceTaskFinish::Cancelled => "cancelled",
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
            TaskReportKind::Failure,
            "abandoned",
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
            TaskReportKind::Presentation,
            "progress",
        ),
        _ => return None,
    };
    Some(ReportContent {
        task_id,
        epoch,
        body,
        kind,
        neutral,
    })
}

impl GovernedRoom {
    fn retained_task_reports(&self) -> Result<BTreeMap<String, RetainedReport>, String> {
        let reports: BTreeMap<String, RetainedReport> = super::private_tasks::read_private(
            &self.path.with_extension("private-task-reports-v1.json"),
        )?
        .unwrap_or_default();
        if reports.len() > 4096 {
            return Err("private task report capacity exhausted".into());
        }
        let mut sequences = BTreeSet::new();
        for (id, retained) in &reports {
            let report = &retained.report;
            report.validate()?;
            if *id != report_key(report.source_device, report.invocation)
                || report.sequence > reports.len() as u64
                || !sequences.insert(report.sequence)
            {
                return Err("invalid private task report ordering".into());
            }
        }
        Ok(reports)
    }

    pub(super) fn load_task_reports(&self) -> Result<BTreeMap<String, TaskReport>, String> {
        let doc = self
            .peer
            .admitted_room()
            .ok_or("private report room absent")?;
        let state = workspace_task::snapshot(doc)?;
        let mut accepted = BTreeMap::new();
        for (id, retained) in self.retained_task_reports()? {
            let report = retained.report;
            if state
                .controls
                .values()
                .flat_map(|control| &control.receipts)
                .any(|receipt| {
                    (receipt.device, receipt.invocation, receipt.request_digest)
                        == (
                            report.source_device,
                            report.invocation,
                            retained.request_digest,
                        )
                })
            {
                accepted.insert(id, report);
            }
        }
        Ok(accepted)
    }

    pub(super) fn prepare_task_report(
        &self,
        command: &C,
        authority: &A,
        invocation: uuid::Uuid,
        request_digest: [u8; 32],
    ) -> Result<(C, Option<TaskReport>), String> {
        let source_device = authority.certificate().device_public_key;
        let reports = self.retained_task_reports()?;
        let retained = reports.get(&report_key(source_device, invocation));
        if retained.is_some_and(|old| old.request_digest != request_digest) {
            return Err("private task report request changed".into());
        }
        let Some(ReportContent {
            task_id,
            epoch,
            body,
            kind,
            neutral,
        }) = report_content(command)
        else {
            return Ok((command.clone(), None));
        };
        let state = workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("private report room absent")?,
        )?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.id == *task_id)
            .ok_or("private report task absent")?;
        if task.route == tilo_desktop_model::WorkspaceTaskRoute::Manual {
            return Ok((command.clone(), None));
        }
        if body.len() > tilo_actions::task_messaging::TaskReportPresentation::MAX_PROSE_BYTES {
            return Err("oversized private task report".into());
        }
        let presentation = match command {
            C::Presentation { presentation, .. } => Some(
                serde_json::from_value::<tilo_actions::task_messaging::TaskReportPresentation>(
                    serde_json::to_value(presentation).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?,
            ),
            _ => None,
        };
        if let Some(presentation) = &presentation {
            presentation.validate()?;
        }
        let mut public = command.clone();
        match &mut public {
            C::PublicProgress { message, .. } => *message = neutral.into(),
            C::Finish { summary, .. } => *summary = neutral.into(),
            C::Abandon { reason, .. } => *reason = neutral.into(),
            C::Presentation { presentation, .. } => {
                presentation.text.clear();
                presentation.reasoning.clear();
                presentation.detail.clear();
            }
            _ => return Err("private report changed variant".into()),
        }
        if let Some(retained) = retained {
            return Ok((public, Some(retained.report.clone())));
        }
        // A canonical retry must still reach the reducer for digest checking,
        // even after the active run ended. Never reconstruct its actor from a
        // later queue assignment or fabricate missing historical prose.
        if workspace_task::result_of(
            self.peer
                .admitted_room()
                .ok_or("private report room absent")?,
            workspace_task::WorkspaceTaskCommandId {
                device: source_device,
                invocation,
            },
        )?
        .is_some()
        {
            return Err("accepted private task report custody absent".into());
        }
        let run = state
            .controls
            .get(task_id)
            .and_then(|control| control.active_run.as_ref())
            .filter(|run| run.epoch == epoch)
            .ok_or("private report does not address the current run")?;
        let actor = match authority {
            A::Agent { agent_id, .. } => format!("agent:{agent_id}"),
            A::Host { .. } => "host".into(),
            A::Person { .. } => return Err("person cannot report an automatic run".into()),
        };
        let report = TaskReport {
            invocation,
            source_device,
            role: TaskReportRole::Assistant,
            accepted_at_unix_ns: u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map_err(|_| "private report clock predates UNIX epoch")?
                    .as_nanos(),
            )
            .map_err(|_| "private report timestamp overflow")?,
            sequence: 1,
            task_run: TaskRunAddress {
                task_id: task_id.clone(),
                run_epoch: epoch,
                agent_id: run.agent_id.clone(),
            },
            actor,
            kind,
            body: body.clone(),
            presentation,
        };
        report.validate()?;
        Ok((public, Some(report)))
    }

    /// Persist after candidate validation but before the public room commit.
    /// `atomic::write_private` syncs both the file and its containing directory.
    pub(super) fn retain_task_report(
        &self,
        mut report: TaskReport,
        request_digest: [u8; 32],
    ) -> Result<(), String> {
        // Validate before any write, including direct custody calls and retries.
        // The wider public presentation type must not bypass the private budget.
        report.validate()?;
        let mut reports = self.retained_task_reports()?;
        let id = report_key(report.source_device, report.invocation);
        if let Some(old) = reports.get(&id) {
            report.sequence = old.report.sequence;
            if old.report != report || old.request_digest != request_digest {
                return Err("private task report receipt changed".into());
            }
            return Ok(());
        }
        if reports.len() >= 4096 {
            return Err("private task report capacity exhausted".into());
        }
        report.sequence = u64::try_from(reports.len())
            .ok()
            .and_then(|sequence| sequence.checked_add(1))
            .ok_or("private task report sequence overflow")?;
        reports.insert(
            id,
            RetainedReport {
                request_digest,
                report,
            },
        );
        let bytes = serde_json::to_vec(&reports).map_err(|_| "private report encoding failed")?;
        tilo_files::native::atomic::write_private(
            &self.path.with_extension("private-task-reports-v1.json"),
            &bytes,
        )
        .map_err(|_| "private report persistence failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::task_messaging::{TaskReportPresentation, TaskReportPresentationFinish};

    #[test]
    fn report_custody_rejects_oversize_without_rewriting_exact_retained_bytes() {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let (key, cert) = super::super::tests::governor();
        let room = GovernedRoom::create(
            directory.path(),
            "Private reports",
            &cert,
            &key,
            std::time::SystemTime::now(),
        )
        .unwrap();
        let presentation = TaskReportPresentation {
            run: "turn:exact".into(),
            sequence: 17,
            received_at: Some(123),
            text: "é".repeat(3072),
            reasoning: "é".repeat(512),
            finish: Some(TaskReportPresentationFinish::Refused),
            detail: "é".repeat(512),
        };
        let report = TaskReport {
            invocation: uuid::Uuid::new_v4(),
            source_device: cert.device_public_key,
            role: TaskReportRole::Assistant,
            accepted_at_unix_ns: 123,
            sequence: 1,
            task_run: TaskRunAddress {
                task_id: "task:exact".into(),
                run_epoch: 1,
                agent_id: "scribe".into(),
            },
            actor: "agent:scribe".into(),
            kind: TaskReportKind::Presentation,
            body: presentation.text.clone(),
            presentation: Some(presentation),
        };
        room.retain_task_report(report.clone(), [7; 32]).unwrap();
        let path = room.path.with_extension("private-task-reports-v1.json");
        let before = std::fs::read(&path).unwrap();
        let retained = room.retained_task_reports().unwrap();
        assert_eq!(
            retained[&report_key(report.source_device, report.invocation)].report,
            report
        );
        for field in ["text", "reasoning", "detail"] {
            let mut oversized = report.clone();
            oversized.invocation = uuid::Uuid::new_v4();
            let presentation = oversized.presentation.as_mut().unwrap();
            match field {
                "text" => {
                    presentation.text.push('x');
                    oversized.body = presentation.text.clone();
                }
                "reasoning" => presentation.reasoning.push('x'),
                _ => presentation.detail.push('x'),
            }
            assert!(
                room.retain_task_report(oversized.clone(), [8; 32]).is_err(),
                "{field}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);

            // Malformed restored custody is also an error, never silently dropped
            // or truncated. A failed load must leave the source file untouched.
            let invalid = BTreeMap::from([(
                report_key(oversized.source_device, oversized.invocation),
                RetainedReport {
                    request_digest: [8; 32],
                    report: oversized,
                },
            )]);
            let invalid_bytes = serde_json::to_vec(&invalid).unwrap();
            std::fs::write(&path, &invalid_bytes).unwrap();
            assert!(room.retained_task_reports().is_err());
            assert_eq!(std::fs::read(&path).unwrap(), invalid_bytes);
            std::fs::write(&path, &before).unwrap();
        }
        room.retain_task_report(report, [7; 32]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
