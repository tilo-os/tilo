//! Mail is reachable only through a registered action admitted by invokeTaskAction.
use super::{CapnpBrokerState, InvocationEnvelope, InvocationOwner, PendingInvocation};
use std::sync::Arc;

impl CapnpBrokerState {
    async fn check_mailbox_wait_run(
        &self,
        room: &str,
        binding: &crate::services::NativeTaskBinding,
        record: &super::DurableInvocation,
    ) -> Result<Option<tokio::time::Instant>, String> {
        let service = self
            .native_tasks(room)
            .ok_or("task inbox owner unavailable")?
            .clone();
        let room = room.to_owned();
        // A remote snapshot may perform a synchronous authenticated RPC. Keep
        // the broker LocalSet free for Stop and other tasks while it is pending.
        let snapshot = tokio::task::spawn_blocking(move || service.snapshot(&room))
            .await
            .map_err(|_| "task inbox owner observation interrupted")??;
        let Some(current) = snapshot
            .controls
            .get(&binding.task_id)
            .and_then(|control| control.active_run.as_ref())
            .filter(|run| run.epoch == binding.epoch && run.agent_id == binding.agent_id)
        else {
            self.cancel_durable_record(record)?;
            return Err("task inbox wait run stopped or replaced".into());
        };
        if current.applied_direction_revision != binding.revision
            || current.approval.as_ref().map(|approval| &approval.person)
                != binding.starter.as_ref()
        {
            return Err("task inbox wait run or instruction frontier changed".into());
        }
        let control = snapshot
            .controls
            .get(&binding.task_id)
            .ok_or("task inbox control absent")?;
        let Some(budget) = control.execution_budget else {
            return Ok(None);
        };
        budget.validate()?;
        // Owner timestamps are Unix seconds. Use the beginning of the recorded
        // second, never a fresh budget for this read or a reconnected agent.
        let started_at = current
            .approval
            .as_ref()
            .map_or(current.started_at, |approval| {
                current.started_at.min(approval.approved_at)
            });
        let started_ns = u64::try_from(started_at)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .ok_or("task inbox run timestamp invalid")?;
        let until_ns = started_ns
            .checked_add(u64::from(budget.total_ms) * 1_000_000)
            .ok_or("task inbox run deadline invalid")?;
        let now = tokio::time::Instant::now();
        let remaining = until_ns.saturating_sub(self.policy_now_ns()?);
        Ok(Some(now + std::time::Duration::from_nanos(remaining)))
    }

    async fn check_mailbox_wait_source(
        self: &Arc<Self>,
        original: &Arc<super::OriginalInvocationData>,
        binding: &crate::services::NativeTaskBinding,
    ) -> Result<(), String> {
        let state = self.clone();
        let original = original.clone();
        let binding = binding.clone();
        // Admission revalidation can synchronously contact the current owner.
        tokio::task::spawn_blocking(move || original.check_native_mailbox_source(&state, &binding))
            .await
            .map_err(|_| "task inbox source observation interrupted")?
    }

    /// A retained read-only child, never a second subscription or a public text
    /// stream. Remote owners use the existing authenticated private inbox read.
    pub(super) async fn execute_task_message_wait(
        self: &Arc<Self>,
        room: &str,
        envelope: &InvocationEnvelope,
        pending: &PendingInvocation,
    ) -> Result<Option<serde_json::Value>, String> {
        use tilo_actions::task_messaging::TaskRunAddress;
        let Some((target, wait)) = crate::workspace_tasks::messaging::wait(envelope)? else {
            return Ok(None);
        };
        let InvocationOwner::NativeTask { agent, task, epoch } = &pending.owner else {
            return Err("task inbox wait requires authenticated native task/run custody".into());
        };
        if target != *task || wait.run_epoch != *epoch {
            return Err("task inbox wait may read only its own exact run".into());
        }
        let key = pending
            .journal_key
            .as_ref()
            .ok_or("task inbox wait durable admission absent")?;
        if key.room != room || key.invocation != envelope.invocation || key.owner != pending.owner {
            return Err("task inbox wait invocation binding changed".into());
        }
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| "native mailbox journal unavailable")?;
            guard
                .as_ref()
                .map_err(Clone::clone)?
                .as_ref()
                .ok_or("native mailbox durable journal absent")?
                .lookup(key)?
                .ok_or("native mailbox admitted read absent")?
        };
        let requirement: crate::services::NativeTaskActionRequirement = serde_json::from_value(
            record
                .intent
                .get("native_requirement")
                .cloned()
                .ok_or("native mailbox exact admitted requirement absent")?,
        )
        .map_err(|_| "native mailbox admitted requirement invalid")?;
        let exact = requirement
            .invocation
            .as_ref()
            .ok_or("native mailbox exact invocation absent")?;
        let binding = requirement
            .binding
            .as_ref()
            .ok_or("native mailbox run binding absent")?;
        if binding.task_id != *task
            || binding.agent_id != *agent
            || binding.epoch != *epoch
            || requirement.app != "so.tilo.desktop"
            || requirement.resource_kind != "workspace-task"
            || requirement.resource != envelope.resource
            || requirement.action != envelope.action
            || requirement.need.field.as_deref() != Some("inbox")
            || exact.id != envelope.invocation.0
            || exact.contract.kind != tilo_actions::ActionKind::Read
        {
            return Err("native mailbox read differs from exact admitted invocation".into());
        }
        let admitted_argument = exact
            .arguments
            .as_deref()
            .ok_or("native mailbox admitted arguments absent")?;
        let admitted_wait: tilo_actions::task_messaging::TaskMessageWait = serde_json::from_str(
            admitted_argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(admitted_argument),
        )
        .map_err(|_| "native mailbox admitted arguments invalid")?;
        if admitted_wait != wait {
            return Err("native mailbox wait differs from admitted exact arguments".into());
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("task inbox wait caller custody absent")?;
        let service = self
            .native_tasks(room)
            .ok_or("task inbox owner unavailable")?
            .clone();
        // Arm both sources before the first authorized read. Watch retains a
        // change that races with that read; no mailbox contents are queued.
        let mut owner_changes = {
            let service = service.clone();
            let room = room.to_owned();
            let binding = binding.clone();
            tokio::task::spawn_blocking(move || service.message_changes_for_run(&room, &binding))
                .await
                .map_err(|_| "task inbox owner subscription interrupted")??
        };
        let mut refresh = self.task_mailbox_refresh.subscribe();
        let mut cancellation = self.task_mailbox_refresh.subscribe();
        // Mailbox children publish no progress: this retained admission instant
        // is the original start, not the time of a transport re-ask.
        let mut deadline = tokio::time::Instant::from_std(pending.last_activity)
            + std::time::Duration::from_millis(u64::from(wait.timeout_ms));
        loop {
            if !self.is_accepting()
                || pending
                    .cancellation_fence
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err("task inbox wait cancelled".into());
            }
            let observation_deadline = deadline;
            let read = async {
                self.check_mailbox_wait_source(original, binding).await?;
                let run_deadline = self.check_mailbox_wait_run(room, binding, &record).await?;
                let read_deadline = run_deadline.map_or(observation_deadline, |until| {
                    observation_deadline.min(until)
                });
                let observe = async {
                    let messages = {
                        let service = service.clone();
                        let room = room.to_owned();
                        let binding = binding.clone();
                        let requirement = requirement.clone();
                        let runtime = tokio::runtime::Handle::current();
                        // Construct the borrowed future on the worker too:
                        // implementations may prepare synchronous owner reads
                        // before returning their asynchronous observation.
                        tokio::task::spawn_blocking(move || {
                            runtime.block_on(service.poll_messages(&room, &binding, &requirement))
                        })
                        .await
                        .map_err(|_| "task inbox owner observation interrupted")??
                    };
                    self.check_mailbox_wait_source(original, binding).await?;
                    let _ = self.check_mailbox_wait_run(room, binding, &record).await?;
                    Ok::<_, String>(messages)
                };
                let messages = tokio::time::timeout_at(read_deadline, observe)
                    .await
                    .map_err(|_| "task inbox owner observation deadline exceeded")??;
                Ok::<_, String>((messages, read_deadline))
            };
            tokio::pin!(read);
            let (messages, read_deadline) = loop {
                tokio::select! {
                    biased;
                    changed = cancellation.changed() => {
                        changed.map_err(|_| "task inbox wait notification closed")?;
                        if !self.is_accepting()
                            || pending.cancellation_fence.load(std::sync::atomic::Ordering::Acquire)
                        {
                            return Err("task inbox wait cancelled".into());
                        }
                    }
                    result = &mut read => break result?,
                    () = tokio::time::sleep_until(deadline) => {
                        // An unavailable owner is not an empty inbox.
                        return Err("task inbox owner observation deadline exceeded".into());
                    }
                }
            };
            deadline = deadline.min(read_deadline);
            let expired = tokio::time::Instant::now() >= deadline;
            let page = crate::workspace_tasks::messaging::wait_page(
                TaskRunAddress {
                    task_id: task.clone(),
                    run_epoch: *epoch,
                    agent_id: agent.clone(),
                },
                wait,
                messages,
                expired,
            )?;
            if pending
                .cancellation_fence
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err("task inbox wait cancelled".into());
            }
            if !page.messages.is_empty() || expired {
                // Private invocation output only. No public document mutation,
                // prose progress, model turn, or DeliveredToModel receipt.
                return serde_json::to_value(page)
                    .map(Some)
                    .map_err(|error| error.to_string());
            }
            // Only a committed-owner or broker hint triggers another private
            // read. The deadline completes the last empty observation, not a
            // fabricated fresh observation or a delivery acknowledgement.
            let expired = tokio::select! {
                biased;
                changed = refresh.changed() => {
                    changed.map_err(|_| "task inbox wait notification closed")?;
                    false
                }
                changed = async {
                    match owner_changes.as_mut() {
                        Some(changes) => changes.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    changed.map_err(|_| "task inbox owner notification closed")?;
                    self.refresh_task_control();
                    false
                }
                () = tokio::time::sleep_until(deadline) => true,
            };
            if expired {
                if !self.is_accepting()
                    || pending
                        .cancellation_fence
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err("task inbox wait cancelled".into());
                }
                // No new owner observation is fabricated at the deadline. The
                // previous authorized empty page remains just an observation.
                self.check_mailbox_wait_source(original, binding).await?;
                if !self.is_accepting()
                    || pending
                        .cancellation_fence
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err("task inbox wait cancelled".into());
                }
                let page = crate::workspace_tasks::messaging::wait_page(
                    TaskRunAddress {
                        task_id: task.clone(),
                        run_epoch: *epoch,
                        agent_id: agent.clone(),
                    },
                    wait,
                    Vec::new(),
                    true,
                )?;
                return serde_json::to_value(page)
                    .map(Some)
                    .map_err(|error| error.to_string());
            }
        }
    }

    pub(super) fn execute_task_message(
        &self,
        room: &str,
        envelope: &InvocationEnvelope,
        pending: &PendingInvocation,
    ) -> Result<Option<serde_json::Value>, String> {
        let Some((recipient, send)) = crate::workspace_tasks::messaging::send(envelope)? else {
            return Ok(None);
        };
        let InvocationOwner::NativeTask { agent, task, epoch } = &pending.owner else {
            return Err("task mail requires authenticated native task/run custody".into());
        };
        let key = pending
            .journal_key
            .as_ref()
            .ok_or("task mail durable admission absent")?;
        if key.room != room || key.invocation != envelope.invocation || key.owner != pending.owner {
            return Err("task mail invocation binding changed".into());
        }
        let service = self
            .native_tasks(room)
            .ok_or("task mail owner unavailable")?;
        let snapshot = service.snapshot(room)?;
        let run = snapshot
            .controls
            .get(task)
            .and_then(|control| control.active_run.as_ref())
            .filter(|run| run.epoch == *epoch && run.agent_id == *agent)
            .ok_or("task mail source run stopped or replaced")?;
        let binding = crate::services::NativeTaskBinding {
            task_id: task.clone(),
            agent_id: agent.clone(),
            epoch: *epoch,
            revision: run.applied_direction_revision,
            starter: run
                .approval
                .as_ref()
                .map(|approval| approval.person.clone()),
        };
        let original = pending
            .original
            .as_ref()
            .ok_or("task mail authenticated caller absent")?;
        original.check_native_mailbox_source(self, &binding)?;
        let requirement: crate::services::NativeTaskActionRequirement = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| "native mailbox journal unavailable")?;
            let journal = guard
                .as_ref()
                .map_err(Clone::clone)?
                .as_ref()
                .ok_or("native mailbox durable journal absent")?;
            let record = journal
                .lookup(key)?
                .ok_or("native mailbox admitted effect absent")?;
            serde_json::from_value(
                record
                    .intent
                    .get("native_requirement")
                    .cloned()
                    .ok_or("native mailbox exact admitted requirement absent")?,
            )
            .map_err(|_| "native mailbox admitted requirement invalid")?
        };
        requirement.check_task_message_payload(&recipient, envelope.invocation.0, &send)?;
        let message = service.send_message(
            room,
            &binding,
            &recipient,
            envelope.invocation.0,
            send,
            &requirement,
        )?;
        self.refresh_task_control();
        // The result contains a receipt, never the private body or another inbox.
        Ok(Some(serde_json::json!({"message_id":message.message_id,
            "source":message.source,"recipient":message.recipient,"status":message.status})))
    }
}
