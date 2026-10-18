//! Native task reports use the already enrolled host and durable owner.
use super::NativeOwnerCommitService;
use tilo_action_broker::services::{
    NativeTaskActionRequirement, NativeTaskBinding, WorkspaceTaskExecution,
};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskSnapshot,
};

pub(super) fn private_text(
    text: tilo_joined_room::NativePrivateTaskInstructions,
) -> tilo_action_broker::services::NativeTaskPrivateInstructions {
    tilo_action_broker::services::NativeTaskPrivateInstructions {
        instruction: text.instruction,
        agent_request: text.agent_request,
        directions: text.directions,
    }
}

impl NativeOwnerCommitService {
    /// A replica's mutation turn cannot serialize against its governor's Stop.
    /// Remote execution therefore reserves and claims at that writer, then
    /// durably consumes its exact permit under the host's queue-fence turn.
    pub(super) fn task_execution_owner(
        &self,
        room: &str,
    ) -> Result<
        (
            super::super::native_workspace_owner::NativeWorkspaceOwner,
            super::super::state::RoomId,
        ),
        String,
    > {
        let room = super::super::state::RoomId::parse(room).ok_or("invalid task room")?;
        let owner = self.opened_owner("native task owner unavailable")?;
        if let Some(network) = &self.network {
            network.check_native_custody(&owner, room)?;
        }
        owner.with_current(room, |view| {
            if view.device() != view.governor() && self.network.is_none() {
                return Err(
                    "native task execution requires a serialized governing-owner admission barrier"
                        .into(),
                );
            }
            Ok(())
        })?;
        Ok((owner, room))
    }
}

impl NativeOwnerCommitService {
    /// Route private exact-run reads through the protected host connection, never
    /// through a replica's public task snapshot or owner-local private store.
    fn private_task_run_read(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        operation: tilo_relay_session::rooms::private::PrivateOperation,
    ) -> Result<Option<tilo_relay_session::rooms::private::PrivateReply>, String> {
        use tilo_relay_session::rooms::private::PrivateOperation;
        let Some(remote) = super::task_mailboxes::remote::route(self, room, None, None)? else {
            return Ok(None);
        };
        super::task_mailboxes::remote::command(
            remote,
            PrivateOperation::NativeTaskRead {
                task: binding.task_id.clone(),
                epoch: binding.epoch,
                agent: binding.agent_id.clone(),
                evidence: Vec::new(),
                operation: Box::new(operation),
            },
        )
        .map(Some)
    }

    fn governor_task_snapshot(
        &self,
        room: super::super::state::RoomId,
        agent: Option<&str>,
    ) -> Result<WorkspaceTaskSnapshot, String> {
        use tilo_joined_room::task_admission::{
            NativeAdmissionCommand as C, NativeAdmissionReply as R,
        };
        match self.native_task_custody(
            room,
            &C::Snapshot {
                agent: agent.map(str::to_owned),
            },
        )? {
            R::Snapshot(snapshot) => {
                let (tasks, controls, grants) = serde_json::from_str(&snapshot)
                    .map_err(|_| "invalid native governor snapshot".to_string())?;
                Ok(WorkspaceTaskSnapshot {
                    tasks,
                    controls,
                    grants,
                })
            }
            _ => Err("unexpected native governor snapshot reply".into()),
        }
    }

    fn admit_custodied<T>(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        caller: Option<&tilo_action_broker::services::NativeTaskAdmissionCaller>,
        dispatch: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        use tilo_joined_room::task_admission::{
            NativeAdmissionCommand as C, NativeAdmissionReply as R, NativeInvocationBinding,
        };
        let (owner, room_id) = self.task_execution_owner(room)?;
        let exact = requirement
            .invocation
            .as_ref()
            .ok_or("native exact invocation absent")?;
        let host = owner.with_current(room_id, |view| Ok(view.device().device_public_key))?;
        let invocation = NativeInvocationBinding {
            room: room_id,
            task: binding.task_id.clone(),
            agent: binding.agent_id.clone(),
            epoch: binding.epoch,
            revision: binding.revision,
            invocation: exact.id,
            request: exact.request,
            host,
            app: requirement.app.clone(),
            provider: exact.provider.clone(),
            resource: requirement.resource.clone(),
            action: requirement.action.clone(),
            arguments_digest: exact.arguments_digest,
            contract_digest: exact.contract_digest,
            ceiling_digest: requirement.ceiling_digest()?,
            private_run_digest: exact.private_run_digest,
        };
        // Retain the view checked under the mutation turn, not a new owner read
        // from inside dispatch. Publication can take long enough for authority
        // to expire, so recheck its clock-sensitive policy immediately before
        // handing the guarded turn to the broker.
        let checked = std::cell::RefCell::new(None);
        let check =
            |view: &tilo_joined_room::NativeCommittedView, exact: &NativeInvocationBinding, now| {
                if let Some(caller) = caller {
                    caller.check(view, requirement, now)?;
                }
                view.check_native_task_action(requirement, exact, now)?;
                *checked.borrow_mut() = Some((view.clone(), exact.clone()));
                Ok(())
            };
        let dispatch = || {
            let (view, exact) = checked
                .borrow_mut()
                .take()
                .ok_or("native task guarded policy view unavailable")?;
            let now = std::time::SystemTime::now();
            if let Some(caller) = caller {
                caller.check(&view, requirement, now)?;
            }
            view.check_native_task_action(requirement, &exact, now)?;
            dispatch()
        };
        let local = owner.with_current(room_id, |view| Ok(view.device() == view.governor()))?;
        if local {
            let (key, governor) = tilo_identity_host::broker::trusted_current_device(room)
                .map_err(|error| error.to_string())?;
            owner.with_current(room_id, |view| {
                if view.governor() != &governor {
                    return Err("native governor signing custody changed".into());
                }
                Ok(())
            })?;
            return owner
                .admit_local_native_invocation(invocation, &governor, &key, check, dispatch)
                .map(|(result, _)| result);
        }
        self.receive_task_fences(room_id)?;
        let permit = {
            self.network
                .as_ref()
                .ok_or("native governor custody transport unavailable")?;
            let R::Reserved(reservation) = self.native_task_custody(
                room_id,
                &C::Reserve {
                    binding: invocation.clone(),
                    requirement: requirement.clone(),
                },
            )?
            else {
                return Err("unexpected governor reservation reply".into());
            };
            let R::Claimed(permit) = self.native_task_custody(
                room_id,
                &C::Claim {
                    reservation,
                    requirement: requirement.clone(),
                },
            )?
            else {
                return Err("unexpected governor permit reply".into());
            };
            permit
        };
        self.receive_task_fences(room_id)?;
        let governor = owner.with_current(room_id, |view| Ok(view.governor().clone()))?;
        owner.admit_reserved_native_invocation_checked(
            &permit,
            &invocation,
            &governor,
            |view, now| check(view, &invocation, now),
            dispatch,
        )
    }
}

impl WorkspaceTaskExecution for NativeOwnerCommitService {
    fn ensure_model_run_open(&self, room: &str, binding: &NativeTaskBinding) -> Result<(), String> {
        use tilo_joined_room::task_admission::{
            NativeAdmissionCommand as C, NativeAdmissionReply as R, NativeTaskAdmissionState as S,
        };
        let (owner, id) = self.task_execution_owner(room)?;
        let local = owner.with_current(id, |view| Ok(view.device() == view.governor()))?;
        let state = if local {
            owner.native_task_admission_state(id, &binding.task_id, binding.epoch)?
        } else {
            let R::State(state) = self.native_task_custody(
                id,
                &C::State {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                },
            )?
            else {
                return Err("unexpected MODEL barrier reply".into());
            };
            state
        };
        if state != S::Open {
            return Err("native MODEL admission is stopped".into());
        }
        Ok(())
    }
    fn requires_room(&self, room: &str) -> bool {
        OwnerCommitService::requires_room(self, room)
    }

    fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String> {
        let (owner, room_id) = self.task_execution_owner(room)?;
        if owner.with_current(room_id, |view| {
            Ok(view.device() != view.governor() && view.device().capabilities.can_act_as_host)
        })? {
            self.receive_task_fences(room_id)?;
            return self.governor_task_snapshot(room_id, None);
        }
        tilo_action_broker::services::WorkspaceMemberships::task_snapshot(self, room)
    }

    fn agent_snapshot(&self, room: &str, agent: &str) -> Result<WorkspaceTaskSnapshot, String> {
        let (owner, room_id) = self.task_execution_owner(room)?;
        if owner.with_current(room_id, |view| Ok(view.device() != view.governor()))? {
            let before = owner.with_current(room_id, |view| {
                Ok((view.device().clone(), view.governor().clone()))
            })?;
            // The synchronous transport runs on the broker's observation worker,
            // never inside with_current: Stop must be able to acquire the owner.
            let snapshot = self.governor_task_snapshot(room_id, Some(agent))?;
            // Resolve current custody again after the await-like transport gap.
            // A response from the former source/device cannot authorize delivery.
            let (current, _) = self.task_execution_owner(room)?;
            current.with_current(room_id, |view| {
                if (view.device().clone(), view.governor().clone()) != before {
                    return Err("native task snapshot custody changed during observation".into());
                }
                Ok(())
            })?;
            return Ok(snapshot);
        }
        owner
            .with_current(room_id, |view| {
                view.peer().workspace().check_task_execution_actor(
                    agent,
                    None,
                    std::time::SystemTime::now(),
                )?;
                Ok(view.generation())
            })
            .and_then(|generation| {
                let snapshot = owner.workspace_task_snapshot(room_id)?;
                owner.with_current(room_id, |view| {
                    if view.generation() != generation {
                        return Err("native task authorization changed; retry the read".into());
                    }
                    Ok(snapshot)
                })
            })
    }

    fn confirmation_invocation(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        asking: uuid::Uuid,
    ) -> Result<uuid::Uuid, String> {
        use tilo_relay_session::rooms::private::{PrivateOperation as O, PrivateReply as R};
        if let Some(reply) = self.private_task_run_read(
            room,
            binding,
            O::TaskConfirmationInvocation {
                task: binding.task_id.clone(),
                epoch: binding.epoch,
                revision: binding.revision,
                asking,
            },
        )? {
            return match reply {
                R::TaskConfirmationInvocation(invocation) => Ok(invocation),
                _ => Err("unexpected native review invocation reply".into()),
            };
        }
        let (owner, room_id) = super::task_mailboxes::owner(self, room)?;
        let authority = super::task_mailboxes::agent_authority(&owner, room_id, binding)?;
        owner.workspace_task_confirmation_invocation(
            room_id,
            binding,
            asking,
            &authority,
            std::time::SystemTime::now(),
        )
    }

    fn private_run_digest(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<[u8; 32], String> {
        use tilo_relay_session::rooms::private::{PrivateOperation, PrivateReply};
        if let Some(reply) = self.private_task_run_read(
            room,
            binding,
            PrivateOperation::TaskRunDigest {
                task: binding.task_id.clone(),
                epoch: binding.epoch,
                revision: binding.revision,
            },
        )? {
            return match reply {
                PrivateReply::TaskRunDigest(digest) => Ok(digest),
                _ => Err("unexpected private run digest reply".into()),
            };
        }
        let (owner, room_id) = super::task_mailboxes::owner(self, room)?;
        let authority = super::task_mailboxes::agent_authority(&owner, room_id, binding)?;
        owner.workspace_task_private_run_digest(
            room_id,
            binding,
            &authority,
            std::time::SystemTime::now(),
        )
    }

    fn private_confirmation(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Option<tilo_desktop_model::WorkspaceTaskConfirmation>, String> {
        use tilo_relay_session::rooms::private::{PrivateOperation, PrivateReply};
        if let Some(reply) = self.private_task_run_read(
            room,
            binding,
            PrivateOperation::TaskConfirmation {
                task: binding.task_id.clone(),
                epoch: binding.epoch,
            },
        )? {
            let PrivateReply::TaskConfirmation(encoded) = reply else {
                return Err("unexpected private confirmation reply".into());
            };
            let confirmation: Option<tilo_desktop_model::WorkspaceTaskConfirmation> = encoded
                .map(|value| {
                    serde_json::from_str(&value)
                        .map_err(|_| "invalid private confirmation".to_string())
                })
                .transpose()?;
            if confirmation
                .as_ref()
                .is_some_and(|held| held.run_epoch != binding.epoch)
            {
                return Err("private confirmation run changed".into());
            }
            return Ok(confirmation);
        }
        let (owner, room_id) = super::task_mailboxes::owner(self, room)?;
        let authority = super::task_mailboxes::agent_authority(&owner, room_id, binding)?;
        owner.workspace_task_private_confirmation(
            room_id,
            &binding.task_id,
            binding.epoch,
            &authority,
            std::time::SystemTime::now(),
        )
    }

    fn freeze_confirmation(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        asking: uuid::Uuid,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        use tilo_joined_room::task_admission::{
            NativeAdmissionCommand as C, NativeAdmissionReply as R,
        };
        if requirement.binding.as_ref().is_none_or(|current| {
            current.task_id != binding.task_id
                || current.agent_id != binding.agent_id
                || current.epoch != binding.epoch
                || current.revision != binding.revision
                || current.starter != binding.starter
        }) {
            return Err("native confirmation run binding differs".into());
        }
        let (owner, room_id) = self.task_execution_owner(room)?;
        let local = owner.with_current(room_id, |view| Ok(view.device() == view.governor()))?;
        if !local {
            return match self.native_task_custody(
                room_id,
                &C::FreezeConfirmation {
                    asking,
                    requirement: requirement.clone(),
                },
            )? {
                R::Acknowledged => Ok(()),
                _ => Err("unexpected governor confirmation freeze reply".into()),
            };
        }
        let (key, host) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| error.to_string())?;
        owner.with_current(room_id, |view| {
            if view.device() != &host || view.governor() != &host {
                return Err("native confirmation signing custody changed".into());
            }
            Ok(())
        })?;
        owner.freeze_native_confirmation(
            room_id,
            asking,
            requirement,
            &host,
            &key,
            std::time::SystemTime::now(),
        )
    }

    fn private_instructions(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Option<tilo_action_broker::services::NativeTaskPrivateInstructions>, String> {
        if let Some(text) = super::task_mailboxes::remote::instructions(self, room, binding)? {
            return Ok(Some(private_text(text)));
        }
        let (owner, room_id) = super::task_mailboxes::owner(self, room)?;
        let authority = super::task_mailboxes::agent_authority(&owner, room_id, binding)?;
        owner
            .workspace_task_private_instructions(
                room_id,
                &binding.task_id,
                binding.epoch,
                &authority,
                std::time::SystemTime::now(),
            )
            .map(|text| text.map(private_text))
    }

    fn messages(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Vec<tilo_actions::task_messaging::TaskMessage>, String> {
        super::task_mailboxes::messages(self, room, binding)
    }

    fn message_changes(
        &self,
        room: &str,
    ) -> Result<Option<tokio::sync::watch::Receiver<()>>, String> {
        let (owner, _) = self.task_execution_owner(room)?;
        Ok(Some(owner.workspace_task_changes()))
    }

    fn message_changes_for_run(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Option<tokio::sync::watch::Receiver<()>>, String> {
        if let Some(changes) = super::task_mailboxes::remote::subscribe(self, room, binding)? {
            return Ok(Some(changes));
        }
        // Do not fall back to an empty joined replica on a failed remote route.
        let (owner, room_id) = super::task_mailboxes::owner(self, room)?;
        super::task_mailboxes::agent_authority(&owner, room_id, binding)?;
        Ok(Some(owner.workspace_task_changes()))
    }

    fn poll_messages(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
    ) -> tilo_action_broker::services::NativeTaskInboxFuture<'_> {
        let (service, room, binding, requirement) = (
            self.clone(),
            room.to_owned(),
            binding.clone(),
            requirement.clone(),
        );
        Box::pin(async move {
            // Remote private RPCs are synchronous at this seam. Never block the
            // broker's LocalSet: it must still serve Stop and other task runs.
            tokio::task::spawn_blocking(move || {
                service.authorize_action(&room, &binding.agent_id, &requirement)?;
                super::task_mailboxes::messages(&service, &room, &binding)
            })
            .await
            .map_err(|_| "native task inbox owner observation interrupted".to_owned())?
        })
    }

    fn report_transcript(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Vec<tilo_actions::task_messaging::TaskReport>, String> {
        super::task_mailboxes::reports(self, room, binding)
    }

    fn record_outputs(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        super::task_mailboxes::record_outputs(self, room, binding, outputs, requirement)
    }

    fn acknowledge_messages(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        ids: &[String],
    ) -> Result<(), String> {
        super::task_mailboxes::acknowledge(self, room, binding, ids)
    }

    fn send_message(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        recipient: &str,
        invocation: uuid::Uuid,
        send: tilo_actions::task_messaging::TaskMessageSend,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<tilo_actions::task_messaging::TaskMessage, String> {
        super::task_mailboxes::send(
            self,
            room,
            binding,
            recipient,
            invocation,
            send,
            requirement,
        )
    }

    fn report(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> tilo_owner_commit::CommitFuture {
        let service = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            let prepare_service = service.clone();
            let request = tokio::task::spawn_blocking(move || {
                let room_id =
                    super::super::state::RoomId::parse(&room).ok_or("invalid task room")?;
                let owner = &prepare_service.opened_owner("native task owner unavailable")?;
                let host = owner.with_current(room_id, |view| {
                    view.device()
                        .capabilities
                        .can_act_as_host
                        .then(|| view.device().clone())
                        .or_else(|| {
                            (view.device().device_public_key == view.governor().device_public_key)
                                .then(|| view.governor().clone())
                        })
                        .ok_or_else(|| "task host attestation unavailable".into())
                })?;
                // Exact retries reuse the owner's retained receipt, across reconnects.
                let bytes = serde_json::to_vec(&(&room, &binding.task_id, binding.epoch, &command))
                    .map_err(|error| error.to_string())?;
                let hash = blake3::hash(&bytes);
                let id = *hash
                    .as_bytes()
                    .first_chunk::<16>()
                    .ok_or("invalid task report digest")?;
                Ok::<_, String>(OwnerCommitRequest {
                    room: room.into(),
                    operation: CommitOperation::WorkspaceTaskCommand {
                        invocation: uuid::Uuid::from_bytes(id),
                        authority: if matches!(command, WorkspaceTaskCommand::Abandon { .. }) {
                            WorkspaceTaskAuthority::Host { attestor: host }
                        } else {
                            WorkspaceTaskAuthority::Agent {
                                attestor: host,
                                agent_id: binding.agent_id,
                            }
                        },
                        command,
                    },
                })
            })
            .await
            .map_err(|_| {
                "native task report preparation interrupted; retry the report".to_owned()
            })??;
            tilo_action_broker::services::WorkspaceMemberships::execute(&service, request).await
        })
    }

    fn authorize_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid task room")?;
        let owner = &self.opened_owner("native task owner unavailable")?;
        if owner.with_current(room_id, |view| Ok(view.device() != view.governor()))? {
            use tilo_joined_room::task_admission::{
                NativeAdmissionCommand as C, NativeAdmissionReply as R,
            };
            return match self.native_task_custody(
                room_id,
                &C::CheckAction {
                    requirement: requirement.clone(),
                },
            )? {
                R::Policy(tilo_action_policy::Answer::Allow) => Ok(()),
                R::Policy(tilo_action_policy::Answer::Ask) => Err(format!(
                    "{}: review this action first",
                    tilo_actions::draft::AWAITING_CONFIRMATION
                )),
                _ => Err("native current policy denies this action".into()),
            };
        }
        owner.with_current(room_id, |view| {
            let now = std::time::SystemTime::now();
            view.peer()
                .workspace()
                .check_task_execution_actor(agent, None, now)?;
            view.check_native_task_preflight(requirement, agent, now)
        })
    }

    fn authorize_retained_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
        invocation: uuid::Uuid,
    ) -> Result<(), String> {
        let (owner, room_id) = self.task_execution_owner(room)?;
        owner.authorize_retained_native_invocation(room_id, invocation, |view, exact, now| {
            view.peer()
                .workspace()
                .check_task_execution_actor(agent, None, now)?;
            view.check_retained_native_task_action(requirement, exact, now)
        })
    }

    fn admit_host_launch(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.admit_custodied(room, binding, requirement, None, launch)
    }

    fn prepare_admission(
        &self,
        room: String,
        binding: NativeTaskBinding,
        requirement: NativeTaskActionRequirement,
        caller: tilo_action_broker::services::NativeTaskAdmissionCaller,
    ) -> tilo_action_broker::services::NativeTaskAdmissionFuture {
        let service = self.clone();
        Box::pin(async move {
            let (mut transfer, ready) =
                tilo_action_broker::services::NativeTaskAdmissionTransfer::channel();
            // Only the owner worker may block. Its callback retains the exact
            // mutation/fence guard while the broker enqueues on its LocalSet.
            tokio::task::spawn_blocking(move || {
                let result =
                    service.admit_custodied(&room, &binding, &requirement, Some(&caller), || {
                        transfer.ready_and_wait()
                    });
                transfer.finish(result);
            });
            ready.await
        })
    }

    fn admit(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<
            tilo_action_broker::collab_broker_capnp::InvocationJob,
            String,
        >,
    ) -> Result<tilo_action_broker::collab_broker_capnp::InvocationJob, String> {
        self.admit_custodied(room, binding, requirement, None, action)
    }
}
