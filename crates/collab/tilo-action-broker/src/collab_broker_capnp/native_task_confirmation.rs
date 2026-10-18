//! A task's action that asks first pauses its run for the person.
//!
//! Before a native task's child starts, its draft is checked against the
//! run's paused confirmation, kept in the room's task controls so a pause
//! outlives the agent, the broker and the host restarting:
//!
//! - no confirmation for these exact arguments, and the grants ask first:
//!   the draft is recorded and the agent is told to wait
//!   ([`tilo_actions::draft::AWAITING_CONFIRMATION`]); nothing runs;
//! - still waiting: the same answer;
//! - confirmed: the child runs with the agent's arguments as the person
//!   changed them, admitted as confirmed, and the confirmation is settled;
//! - declined: the agent is told, with the person's reason
//!   ([`tilo_actions::draft::DECLINED`]), and the confirmation is settled.
//!
//! An agent never confirms for itself: what it sends is only ever matched
//! against what its person answered.

use tilo_actions::Draft;
use tilo_desktop_model::{WorkspaceTaskConfirmation, WorkspaceTaskConfirmationState};
use tilo_peer_state::workspace_task::WorkspaceTaskCommand;

use super::native_task_execution::ChildReask;
use super::{CapnpBrokerState, PendingTaskRun, PublishedAction};

/// How a native task's child may start.
pub(super) enum NativeGate {
    /// As requested: nothing asks first.
    Proceed,
    /// The person confirmed it: with these arguments, as they changed them,
    /// admitted as confirmed. The confirmation is settled once it starts.
    Confirmed {
        invocation: uuid::Uuid,
        argument: String,
        /// Unique answer custody; the stable draft id below is only for cleanup.
        reference: String,
        confirmation: String,
    },
}

/// Exact transport identity shared with the final child admission.
pub(super) struct ConfirmationContext<'a> {
    pub lease: &'a str,
    pub invocation: uuid::Uuid,
    pub provider: &'a str,
}

type ConfirmationArguments<'a> = (
    serde_json::Map<String, serde_json::Value>,
    Option<&'a str>,
    [u8; 32],
);

/// The agent's arguments as the wire form every caller uses: JSON behind
/// `ARGUMENTS_JSON_PREFIX`, a bare object read the same.
fn native_confirmation_arguments(
    argument: Option<&str>,
) -> Result<ConfirmationArguments<'_>, String> {
    let prefixed = argument.and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX));
    let arguments = match prefixed.or(argument) {
        Some(text) => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text)
            .map_err(|_| "task action arguments are not a JSON object")?,
        None => serde_json::Map::new(),
    };
    let digest = tilo_actions::draft::arguments_digest(&arguments);
    Ok((arguments, prefixed, digest))
}

impl CapnpBrokerState {
    /// Resolve private execution content through the authenticated owner service.
    pub(super) fn native_private_run_digest(
        &self,
        run: &PendingTaskRun,
        binding: &crate::services::NativeTaskBinding,
    ) -> Result<[u8; 32], String> {
        let digest = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?
            .private_run_digest(&run.cluster_id, binding)?;
        if digest == [0; 32] {
            return Err("native task private run digest absent".into());
        }
        Ok(digest)
    }

    /// Decide how `reask`'s child may start, recording the draft for the
    /// person when it asks first.
    ///
    /// # Errors
    /// The child waits for the person, the person declined it, or the run
    /// or action can no longer be read.
    pub(super) async fn native_confirmation_gate(
        &self,
        reask: &ChildReask<'_>,
    ) -> Result<NativeGate, String> {
        let run = self.task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)?;
        if run.native.is_none() {
            return Ok(NativeGate::Proceed);
        }
        let target = reask.target;
        let (surface, offered) = self
            .surfaces
            .lock()
            .map_err(|_| "task action surface unavailable")?
            .get(&target.peer_id)
            .filter(|surface| surface.route.cluster_id == run.cluster_id)
            .and_then(|surface| surface.action_projection(target.resource, target.action))
            .ok_or("the task action is no longer published in this workspace")?;
        let provider = if target.provider.is_empty() {
            super::launchable::launchable_provider(&surface)
        } else {
            target.provider.to_owned()
        };
        self.native_confirmation_for(
            &run,
            ConfirmationContext {
                lease: reask.lease,
                invocation: reask.invocation.0,
                provider: &provider,
            },
            (&surface.app_id, &offered),
            (target.resource, target.action),
            target.argument,
        )
        .await
    }

    /// [`Self::native_confirmation_gate`] for `offered`, an action of `app`
    /// on `resource`, whether it is published by the running application or
    /// retained from a closed one: a task action that asks first on its way
    /// to an application being launched pauses at a reviewable draft too.
    ///
    /// # Errors
    /// As [`Self::native_confirmation_gate`].
    pub(super) async fn native_confirmation_for(
        &self,
        run: &PendingTaskRun,
        context: ConfirmationContext<'_>,
        (app, offered): (&str, &PublishedAction),
        (resource, action): (&str, &str),
        argument: Option<&str>,
    ) -> Result<NativeGate, String> {
        let Some(mut binding) = run.native.clone() else {
            return Ok(NativeGate::Proceed);
        };
        binding.revision = runtime_collab::native_task_revision(context.lease)
            .ok_or("invalid native lease")?
            .1;
        let (arguments, prefixed, digest) = native_confirmation_arguments(argument)?;
        let json = prefixed.or(argument);
        let descriptor = offered.descriptor(tilo_actions::ExecutionLocus::ClientLocal);
        // Named by the resource itself, not the window alias it is invoked
        // through: a run resumed after a restart asks through a new window
        // and must find the draft the person was already shown.
        let id = WorkspaceTaskConfirmation::id_for(
            &tilo_actions::selection::logical_resource_id(resource),
            action,
            &digest,
        );
        let service = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?;
        let held = service
            .private_confirmation(&run.cluster_id, &binding)?
            .filter(|held| held.id == id && held.run_epoch == binding.epoch);
        if let Some(held) = held {
            return self
                .held_answer(
                    run,
                    held,
                    &arguments,
                    (prefixed.is_some(), binding.revision),
                )
                .await;
        }
        let contract_argument =
            json.map(|text| format!("{}{text}", tilo_actions::ARGUMENTS_JSON_PREFIX));
        let consequential = !matches!(
            descriptor
                .contract
                .confirmation_for_argument(contract_argument.as_deref()),
            tilo_actions::ConfirmationPolicy::None
        );
        let mut current = binding.clone();
        current.revision = runtime_collab::native_task_revision(context.lease)
            .ok_or("invalid native lease")?
            .1;
        let mut invocation = crate::services::NativeTaskInvocation::new(
            context.invocation,
            context.provider,
            argument,
            &descriptor.contract,
            context.lease,
        )?;
        invocation.private_run_digest = self.native_private_run_digest(run, &current)?;
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: Some(current),
            app: app.to_owned(),
            resource_kind: descriptor.resource_kind.clone(),
            resource: tilo_actions::selection::logical_resource_id(resource).into_owned(),
            action: descriptor.address.action.clone(),
            need: descriptor.contract.need(),
            consequential,
            confirmation: None,
            invocation: Some(invocation),
        };
        match service.authorize_action(&run.cluster_id, &run.agent_id, &requirement) {
            Ok(()) => Ok(NativeGate::Proceed),
            Err(error)
                if crate::services::NativeTaskActionRequirement::awaits_confirmation(&error) =>
            {
                let confirmation = WorkspaceTaskConfirmation {
                    id,
                    asking: uuid::Uuid::new_v4(),
                    run_epoch: binding.epoch,
                    agent_id: run.agent_id.clone(),
                    app: app.to_owned(),
                    resource_kind: descriptor.resource_kind.clone(),
                    resource: tilo_actions::selection::logical_resource_id(resource).into_owned(),
                    action: descriptor.address.action.clone(),
                    kind: descriptor.contract.kind,
                    risk: descriptor.contract.risk,
                    draft: Draft::render(&descriptor, &arguments),
                    arguments_digest: digest,
                    requested_at: unix_seconds(),
                    state: WorkspaceTaskConfirmationState::Waiting,
                };
                service.freeze_confirmation(
                    &run.cluster_id,
                    requirement
                        .binding
                        .as_ref()
                        .ok_or("native task binding absent")?,
                    confirmation.asking,
                    &requirement,
                )?;
                Err(self.ask_native_confirmation(run, confirmation).await)
            }
            Err(error) => Err(error),
        }
    }

    /// What the person's answer `held` lets the child with `arguments` do.
    /// The confirmed arguments keep the form the agent sent them in
    /// (`prefixed` behind `ARGUMENTS_JSON_PREFIX`, else a bare object).
    async fn held_answer(
        &self,
        run: &PendingTaskRun,
        held: WorkspaceTaskConfirmation,
        arguments: &serde_json::Map<String, serde_json::Value>,
        (prefixed, revision): (bool, u64),
    ) -> Result<NativeGate, String> {
        match &held.state {
            WorkspaceTaskConfirmationState::Waiting => Err(awaiting(&held.draft)),
            WorkspaceTaskConfirmationState::Confirmed { edits, .. } => {
                let edited = held.draft.apply_edits(arguments, edits)?;
                let edited = serde_json::Value::Object(edited).to_string();
                let mut binding = run.native.clone().ok_or("native task binding absent")?;
                binding.revision = revision;
                let invocation = self
                    .native_tasks(&run.cluster_id)
                    .ok_or("native task owner unavailable")?
                    .confirmation_invocation(&run.cluster_id, &binding, held.asking)?;
                Ok(NativeGate::Confirmed {
                    invocation,
                    argument: if prefixed {
                        format!("{}{edited}", tilo_actions::ARGUMENTS_JSON_PREFIX)
                    } else {
                        edited
                    },
                    reference: held.asking.to_string(),
                    confirmation: held.id,
                })
            }
            WorkspaceTaskConfirmationState::Declined { reason, .. } => {
                self.settle_native_confirmation(run, &held.id).await?;
                let declined = format!(
                    "{}: the person declined {}",
                    tilo_actions::draft::DECLINED,
                    held.draft.action_label
                );
                Err(match reason {
                    Some(reason) => format!("{declined}: {reason}"),
                    None => declined,
                })
            }
        }
    }

    /// Pause `run` at `confirmation` for its person; the error the child is
    /// answered with.
    async fn ask_native_confirmation(
        &self,
        run: &PendingTaskRun,
        confirmation: WorkspaceTaskConfirmation,
    ) -> String {
        let Some((binding, service)) = run.native.clone().zip(self.native_tasks(&run.cluster_id))
        else {
            return "native task owner unavailable".into();
        };
        let waiting = awaiting(&confirmation.draft);
        let reported = service
            .report(
                &run.cluster_id,
                binding.clone(),
                WorkspaceTaskCommand::RequestConfirmation {
                    task_id: run.task_id.clone(),
                    run_epoch: binding.epoch,
                    confirmation: Box::new(confirmation),
                },
            )
            .await;
        match reported {
            Ok(receipt) if receipt.binding.room == run.cluster_id => waiting,
            Ok(_) => "native task confirmation receipt room differs".into(),
            Err(error) => error,
        }
    }

    /// Clear the run's answered confirmation `id`: its answer reached the
    /// run.
    pub(super) async fn settle_native_confirmation(
        &self,
        run: &PendingTaskRun,
        id: &str,
    ) -> Result<(), String> {
        let binding = run.native.clone().ok_or("not a native task run")?;
        let service = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?;
        service
            .report(
                &run.cluster_id,
                binding.clone(),
                WorkspaceTaskCommand::SettleConfirmation {
                    task_id: run.task_id.clone(),
                    run_epoch: binding.epoch,
                    confirmation_id: id.to_owned(),
                },
            )
            .await
            .map(drop)
    }
}

fn awaiting(draft: &Draft) -> String {
    format!(
        "{}: {} waits for the person to confirm it in Task Center",
        tilo_actions::draft::AWAITING_CONFIRMATION,
        draft.action_label
    )
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}
