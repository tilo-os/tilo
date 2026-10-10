//! Steering on the existing native task lease. Receipt alone never acknowledges it.
use super::{
    CollabEvent, CollabRequest, Duration, Result, Session, StepOutcome, TASK_ACTION_WAIT,
    TaskAction, TaskActionStop, Uuid,
};

#[derive(Default)]
pub(super) struct NativeTaskState {
    pub run_id: u64,
    pub task_id: String,
    pub base: String,
    pub revision: u64,
    pub ordinal: u32,
    pub epoch: Option<u64>,
    pub pending: Option<runtime_collab::TaskRunControl>,
}

impl Session {
    pub(super) fn follow_native_invocation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
        invocation: tilo_actions::InvocationId,
        mut update: tilo_actions::InvocationUpdate,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        let lease = self.task_lease(run);
        let deadline = self.task_wait_deadline(TASK_ACTION_WAIT);
        while !update.is_terminal() && ui::time::now() < deadline {
            if self.task_cancelled(run)? {
                // The same status request asks the host's existing cooperative
                // cancellation system to stop cancellable children of this run.
                return Ok(Err(TaskActionStop::Cancelled(Some(
                    "Cancelled from Task Center".into(),
                ))));
            }
            let request_id = self.next_request_id();
            self.link.send(&CollabRequest::InvokeTaskAction {
                request_id,
                invocation,
                run_id: run.run_id,
                lease: lease.clone(),
                step: None,
                peer_id: action.peer_id,
                resource: action.resource.clone(),
                action: action.action.clone(),
                arguments: arguments.clone(),
            })?;
            update = match self.wait_for_task_action_result(request_id)? {
                Ok((_, update)) => update,
                Err(error) => return Ok(Err(TaskActionStop::Failed(error))),
            };
            std::thread::sleep(Duration::from_millis(50));
        }
        match update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                Ok(Ok(StepOutcome { output }))
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Ok(Err(TaskActionStop::Cancelled(message)))
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                Ok(Err(TaskActionStop::Failed(error.message)))
            }
            _ => Ok(Err(TaskActionStop::Failed(
                "Native action outcome remains unknown; review history before retrying".into(),
            ))),
        }
    }
    pub(super) fn wait_for_native_report(&mut self, request_id: u64) -> Result<()> {
        let deadline = self.task_wait_deadline(Duration::from_secs(5));
        let mut deferred = Vec::new();
        while ui::time::now() < deadline {
            match self.link.recv(Duration::from_millis(50))? {
                Some(CollabEvent::TaskRunReportResult {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return result.map_err(anyhow::Error::msg);
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        anyhow::bail!("native task report has no durable acknowledgement")
    }
    pub(super) fn task_lease(&self, run: &runtime_collab::TaskRunEnvelope) -> String {
        match self
            .native_task
            .as_ref()
            .filter(|state| state.run_id == run.run_id)
        {
            Some(state) => format!("{}:{}", state.base, state.revision),
            None => run.lease.clone(),
        }
    }

    pub(super) fn observe_native_control(
        &mut self,
        control: runtime_collab::TaskRunControl,
    ) -> Result<()> {
        control.validate().map_err(anyhow::Error::msg)?;
        let state = self
            .native_task
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("unexpected native task control"))?;
        if control.task_id != state.task_id
            || state.epoch.is_some_and(|epoch| epoch != control.run_epoch)
        {
            anyhow::bail!("native task control belongs to another run or frontier");
        }
        if control.from_revision != state.revision {
            anyhow::bail!("native task control belongs to another instruction frontier");
        }
        state.epoch = Some(control.run_epoch);
        if control.has_more {
            anyhow::bail!(
                "More than one page of task directions needs person review; none were acknowledged"
            );
        }
        if !control.directions.is_empty() {
            state.pending = Some(control);
        }
        Ok(())
    }

    pub(super) fn incorporate_native_directions(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        conversation: &mut crate::model::Conversation,
    ) -> Result<()> {
        let Some(control) = self
            .native_task
            .as_mut()
            .and_then(|state| state.pending.take())
        else {
            return Ok(());
        };
        // The attributed, exact source is now in the next model request. A
        // concurrent newer direction still fences every child at the owner.
        let text = serde_json::to_string(&control.directions)?;
        conversation.push_user(&format!(
            "Authenticated workspace task directions (apply before further operations):\n{text}"
        ));
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::DirectionsApplied {
                revision: control.latest_revision,
            },
        )?;
        if let Some(state) = self.native_task.as_mut() {
            state.revision = control.latest_revision;
        }
        self.step_settled();
        // This round trip observes durable acknowledgement; logging a failed
        // report in the SDK cannot silently mint a usable revised lease.
        if self.task_cancelled(run)? {
            anyhow::bail!("task cancelled while incorporating directions");
        }
        Ok(())
    }

    pub(super) fn task_child_id(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
    ) -> Result<tilo_actions::InvocationId> {
        let Some(state) = self
            .native_task
            .as_mut()
            .filter(|state| state.run_id == run.run_id)
        else {
            return Ok(tilo_actions::InvocationId::new());
        };
        let revision = u32::try_from(state.revision)?;
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&run.run_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&revision.to_le_bytes());
        bytes[12..].copy_from_slice(&state.ordinal.to_le_bytes());
        state.ordinal = state
            .ordinal
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("native task step index exhausted"))?;
        Ok(tilo_actions::InvocationId(Uuid::from_bytes(bytes)))
    }
}
