//! A task's step paused for the person's confirmation.
//!
//! Where an operation asks first, the host answers the step
//! [`tilo_actions::draft::AWAITING_CONFIRMATION`] and shows the person the
//! step's draft on the issue. The step waits: it says so in the task's
//! progress and asks once more with the identical request, which the host
//! holds until the person answers, a grant that decides the step changes or
//! the run stops; it runs the step once the person confirmed it, as they
//! edited it. Time spent waiting does not count against the task's time
//! budget. A decline ([`tilo_actions::draft::DECLINED`]) ends the step with
//! the person's reason, for the model to plan around.

use anyhow::Result;
use ui::time::Instant;

use super::{CollabRequest, Session, TaskAction, TaskActionStop};

/// Whether the host's `error` says the step waits for the person.
pub(super) fn is_awaiting(error: &str) -> bool {
    error.contains(tilo_actions::draft::AWAITING_CONFIRMATION)
}

/// The host's `error` as the step's outcome: a decline in the person's
/// words, with what to do next; anything else as it came.
pub(super) fn step_failure(error: String) -> String {
    let marker = format!("{}: ", tilo_actions::draft::DECLINED);
    match error.find(&marker) {
        Some(at) => {
            let said = error
                .get(at.saturating_add(marker.len())..)
                .unwrap_or_default()
                .trim();
            format!(
                "{said}. The person declined this step: do not retry it; continue without it or \
                 end the task, and say so in your reply"
            )
        }
        None => error,
    }
}

impl Session {
    /// Wait for the person to answer the paused step `action` with
    /// `arguments` (child `invocation`): one ask, held by the host until the
    /// step runs, is declined or the run is cancelled.
    pub(super) fn await_confirmation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (action, arguments): (&TaskAction, &serde_json::Map<String, serde_json::Value>),
        invocation: tilo_actions::InvocationId,
    ) -> Result<Result<tilo_actions::InvocationUpdate, TaskActionStop>> {
        let paused = ui::time::now();
        tracing::info!(action = %action.action, "task step waits for confirmation");
        self.ask_on_tracker(run, (action, arguments))?;
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: format!("Waiting for your confirmation: {}", action.label),
                presentation: None,
            },
        )?;
        let request_id = self.next_request_id();
        self.link.send(&CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id: run.run_id,
            lease: self.task_lease(run),
            peer_id: action.peer_id,
            provider: action.provider.clone(),
            resource: action.resource.clone(),
            action: action.action.clone(),
            arguments: arguments.clone(),
            wait: Some(runtime_collab::TaskChildWait {
                wait_ms: u32::MAX,
                seen: tilo_actions::remote::UpdateMark::Accepted,
            }),
        })?;
        // Waiting for the person does not count against the task's time
        // budget, so neither does this answer's.
        let outcome = match self.task_action_result_until(request_id, None)? {
            Ok((_, update)) => Ok(update),
            Err(_) if self.task_cancelled(run)? => Err(TaskActionStop::Cancelled(Some(
                "Cancelled while waiting for confirmation".into(),
            ))),
            Err(error) => Err(TaskActionStop::Failed(step_failure(error))),
        };
        self.extend_deadline(paused);
        if outcome.is_ok() {
            self.settle_on_tracker(run, (action, arguments))?;
        }
        Ok(outcome)
    }

    /// Give the task's time budget back what waiting since `paused` took.
    fn extend_deadline(&mut self, paused: Instant) {
        let waited = ui::time::now().saturating_duration_since(paused);
        if let Some(deadline) = self.task_deadline.as_mut()
            && let Some(later) = deadline.checked_add(waited)
        {
            *deadline = later;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decline_reaches_the_model_in_the_persons_words() {
        let error = "Failed: action_declined: the person declined Send message: Not to Bob";
        let said = step_failure(error.into());
        assert!(said.starts_with("the person declined Send message: Not to Bob."));
        assert!(said.contains("do not retry it"));
        assert!(is_awaiting(
            "Failed: action_awaiting_confirmation: Send message waits for the person"
        ));
        assert!(!is_awaiting(error));
        assert_eq!(step_failure("timeout".into()), "timeout");
    }
}
