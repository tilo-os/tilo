//! A task's step paused for the person's confirmation.
//!
//! Where an operation asks first, the host answers the step
//! [`tilo_actions::draft::AWAITING_CONFIRMATION`] and shows the person the
//! step's draft in Task Center. The step waits: it says so in the task's
//! progress, asks again with the identical request every few seconds (the
//! host runs it once the person confirmed it, as they edited it), and stops
//! when the run is cancelled. Time spent waiting does not count against the
//! task's time budget. A decline ([`tilo_actions::draft::DECLINED`]) ends the
//! step with the person's reason, for the model to plan around.

use anyhow::Result;
use ui::time::{Duration, Instant};

use super::{CollabRequest, Session, TaskAction, TaskActionStop};

/// How long a paused step waits between asking again.
const ASK_AGAIN_EVERY: Duration = Duration::from_secs(2);

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
    /// `arguments` (child `invocation`), asking the host again until it runs,
    /// is declined or the run is cancelled.
    pub(super) fn await_confirmation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (action, arguments): (&TaskAction, &serde_json::Map<String, serde_json::Value>),
        invocation: tilo_actions::InvocationId,
    ) -> Result<Result<tilo_actions::InvocationUpdate, TaskActionStop>> {
        let paused = ui::time::now();
        tracing::info!(action = %action.action, "task step waits for confirmation");
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: format!("Waiting for your confirmation: {}", action.label),
                presentation: None,
            },
        )?;
        let outcome = loop {
            self.idle(ASK_AGAIN_EVERY);
            if self.task_cancelled(run)? {
                break Err(TaskActionStop::Cancelled(Some(
                    "Cancelled while waiting for confirmation".into(),
                )));
            }
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
                wait: None,
            })?;
            self.extend_deadline(paused);
            match self.wait_for_task_action_result(request_id)? {
                Ok((_, update)) => break Ok(update),
                Err(error) if is_awaiting(&error) => {}
                Err(error) => break Err(TaskActionStop::Failed(step_failure(error))),
            }
        };
        self.extend_deadline(paused);
        Ok(outcome)
    }

    /// Keep the model turn and the link moving for `wait`, holding what
    /// arrives for whoever reads it next.
    fn idle(&mut self, wait: Duration) {
        let until = ui::time::now()
            .checked_add(wait)
            .unwrap_or_else(ui::time::now);
        let mut deferred = Vec::new();
        while ui::time::now() < until {
            self.pump_turn();
            let left = until.saturating_duration_since(ui::time::now());
            match self.link.recv(left.min(Duration::from_millis(100))) {
                Ok(Some(event)) => deferred.push(event),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        self.link.defer(deferred);
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
