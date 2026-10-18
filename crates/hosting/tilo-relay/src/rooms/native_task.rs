//! Private native task coordination and exact invocation custody at the governor.
//! All methods run under the governed room writer lock, including Stop.
use super::{GovernedRoom, Rooms};
use std::time::SystemTime;
use tilo_joined_room::task_admission::{
    NativeAdmissionCommand as C, NativeAdmissionReply as R, NativeGovernorAdmission,
    TaskAdmissionLedger,
};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority as A, WorkspaceTaskCommand as T};
use tilo_user_identity::DeviceCert;

impl GovernedRoom {
    pub(super) fn load_native_admission(&self) -> Result<TaskAdmissionLedger, String> {
        let ledger: TaskAdmissionLedger = super::private_tasks::read_private(
            &self.path.with_extension("native-task-admission-v1.json"),
        )?
        .unwrap_or_default();
        let id = tilo_desktop_model::RoomId::parse(self.room()).ok_or("invalid native room")?;
        ledger.verify_retained(id, self.peer.authority().device_public_key)?;
        Ok(ledger)
    }
    fn persist_native_admission(&self, ledger: &TaskAdmissionLedger) -> Result<(), String> {
        let bytes = serde_json::to_vec(ledger).map_err(|_| "native custody encoding failed")?;
        if bytes.len() > 72 * 1024 * 1024 {
            return Err("native custody exceeds capacity".into());
        }
        tilo_files::native::atomic::write_private(
            &self.path.with_extension("native-task-admission-v1.json"),
            &bytes,
        )
        .map_err(|_| "native custody persistence failed".into())
    }
    pub(super) fn native_host(&self, caller: &DeviceCert) -> Result<(), String> {
        if (!caller.capabilities.can_act_as_host && caller != self.peer.authority())
            || caller.user_public_key != self.peer.authority().user_public_key
            || !self.peer.certificates().contains(caller)
        {
            return Err("native task requires current owner-certified physical host".into());
        }
        let now = SystemTime::now();
        let now_ns = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "native host clock predates epoch")?
            .as_nanos()
            .try_into()
            .map_err(|_| "native host clock exceeds range")?;
        tilo_user_identity::verify_device_cert_at(caller, now_ns)
            .map_err(|_| "native host certificate invalid")?;
        if self
            .peer
            .permissions(caller.device_public_key, now)?
            .delegate
            .is_some()
        {
            return Err("native task host cannot be a delegated agent".into());
        }
        Ok(())
    }
    fn check_native_private_run(
        &self,
        caller: &DeviceCert,
        requirement: &tilo_joined_room::task_policy::NativeTaskActionRequirement,
    ) -> Result<(), String> {
        use tilo_relay_session::rooms::private::{PrivateOperation as O, PrivateReply as P};
        let run = requirement
            .binding
            .as_ref()
            .ok_or("native run binding absent")?;
        let invocation = requirement
            .invocation
            .as_ref()
            .ok_or("native invocation absent")?;
        let reply = self.private_task_command(
            caller,
            O::NativeTaskRead {
                task: run.task_id.clone(),
                epoch: run.epoch,
                agent: run.agent_id.clone(),
                evidence: Vec::new(),
                operation: Box::new(O::TaskRunDigest {
                    task: run.task_id.clone(),
                    epoch: run.epoch,
                    revision: run.revision,
                }),
            },
        )?;
        if !matches!(reply, P::TaskRunDigest(digest) if digest == invocation.private_run_digest) {
            return Err("native private run snapshot changed".into());
        }
        Ok(())
    }

    fn check_model_private_run(
        &self,
        caller: &DeviceCert,
        context: &tilo_joined_room::NativeModelContext,
    ) -> Result<(), String> {
        use tilo_relay_session::rooms::private::{PrivateOperation as O, PrivateReply as P};
        self.native_host(caller)?;
        let reply = self.private_task_command(
            caller,
            O::NativeTaskRead {
                task: context.task.clone(),
                epoch: context.epoch,
                agent: context.actor.clone(),
                evidence: Vec::new(),
                operation: Box::new(O::TaskRunDigest {
                    task: context.task.clone(),
                    epoch: context.epoch,
                    revision: context.revision,
                }),
            },
        )?;
        if !matches!(reply, P::TaskRunDigest(digest) if digest == context.private_run_digest) {
            return Err("native MODEL private run snapshot changed".into());
        }
        Ok(())
    }

    fn custody(
        &self,
        caller: &DeviceCert,
        command: C,
        key: &ed25519_dalek::SigningKey,
    ) -> Result<R, String> {
        let now = SystemTime::now();
        if !self.peer.certificates().contains(caller) {
            return Err("native caller certificate is not current".into());
        }
        self.peer.permissions(caller.device_public_key, now)?;
        let id = tilo_desktop_model::RoomId::parse(self.room()).ok_or("invalid native room")?;
        let mut ledger = self.load_native_admission()?;
        let snapshot = self.task_policy_snapshot()?;
        match &command {
            C::Reserve {
                binding,
                requirement,
            } => {
                self.check_native_private_run(caller, requirement)?;
                ledger.check_action(&self.peer, &snapshot, requirement, binding, now)?;
            }
            C::Claim {
                reservation,
                requirement,
            } => {
                self.check_native_private_run(caller, requirement)?;
                ledger.check_action(
                    &self.peer,
                    &snapshot,
                    requirement,
                    &reservation.binding,
                    now,
                )?;
            }
            C::RegisterModelHost(registration) => {
                self.check_model_private_run(caller, registration.context())?;
            }
            C::ReserveModel {
                registration,
                requirement,
            }
            | C::ClaimModel {
                registration,
                requirement,
                ..
            } => {
                self.native_host(caller)?;
                let current = ledger.check_model_host(*registration, requirement, caller)?;
                self.check_model_private_run(caller, &current)?;
                requirement.check_registered_context(
                    &self.peer,
                    &snapshot,
                    &requirement.binding()?,
                    &current,
                    now,
                )?;
            }
            C::FreezeConfirmation { requirement, .. } | C::CheckAction { requirement } => {
                self.native_host(caller)?;
                self.check_native_private_run(caller, requirement)?;
            }
            _ => {}
        }
        let mut admission = NativeGovernorAdmission {
            room: id,
            peer: &self.peer,
            ledger: &mut ledger,
            continuity: self.record.generation,
            key,
        };
        let reply = match command {
            C::ModelDelivery(receipt) => {
                self.native_host(caller)?;
                if receipt.room() != id {
                    return Err("MODEL delivery room changed".into());
                }
                ledger.accept_model_delivery(&receipt, caller.device_public_key)?;
                // Retain authenticated provenance before its derived mailbox fact.
                // Replay repairs a crash between these two private file writes.
                self.persist_native_admission(&ledger)?;
                self.persist_model_delivery(&receipt)?;
                R::Acknowledged
            }
            C::RegisterModelHost(registration) => {
                admission.register_model_host(&registration, caller, now)?;
                R::Acknowledged
            }
            C::RevokeModelHost { registration } => {
                admission.revoke_model_host(registration, caller, now)?;
                R::Acknowledged
            }
            C::ReserveModel {
                registration,
                requirement,
            } => {
                let current =
                    admission
                        .ledger
                        .check_model_host(registration, &requirement, caller)?;
                R::Reserved(admission.reserve(
                    requirement.binding()?,
                    caller,
                    now,
                    |peer, exact| {
                        requirement.check_registered_context(peer, &snapshot, exact, &current, now)
                    },
                )?)
            }
            C::ClaimModel {
                registration,
                requirement,
                reservation,
            } => {
                let current =
                    admission
                        .ledger
                        .check_model_host(registration, &requirement, caller)?;
                // Claim must bind the requirement itself, not merely share a run.
                if requirement.binding()? != reservation.binding {
                    return Err("native MODEL reservation requirement changed".into());
                }
                R::Claimed(admission.claim(&reservation, caller, now, |peer, exact| {
                    requirement.check_registered_context(peer, &snapshot, exact, &current, now)
                })?)
            }
            C::Reserve {
                binding,
                requirement,
            } => {
                if binding.room != id {
                    return Err("native custody room changed".into());
                }
                R::Reserved(admission.reserve(binding, caller, now, |peer, exact| {
                    requirement.check_invocation_with_snapshot(peer, &snapshot, exact, now)
                })?)
            }
            C::Claim {
                reservation,
                requirement,
            } => {
                if reservation.binding.room != id {
                    return Err("native custody room changed".into());
                }
                R::Claimed(admission.claim(&reservation, caller, now, |peer, exact| {
                    requirement.check_invocation_with_snapshot(peer, &snapshot, exact, now)
                })?)
            }
            C::FreezeConfirmation {
                asking,
                requirement,
            } => {
                ledger.freeze_confirmation(
                    &self.peer,
                    &snapshot,
                    asking,
                    &requirement,
                    caller,
                    now,
                )?;
                R::Acknowledged
            }
            C::CheckAction { requirement } => {
                let run = requirement
                    .binding
                    .as_ref()
                    .ok_or("native binding absent")?;
                let answer = match requirement.check_with_snapshot(&self.peer, &snapshot, self.room(), &run.agent_id, now) {
                    Ok(()) => tilo_action_policy::Answer::Allow,
                    Err(error) if tilo_joined_room::task_policy::NativeTaskActionRequirement::awaits_confirmation(&error) => tilo_action_policy::Answer::Ask,
                    Err(_) => tilo_action_policy::Answer::Deny,
                };
                R::Policy(answer)
            }
            C::FenceAcknowledgement(ack) => {
                if ack.fence.room != id {
                    return Err("native fence room changed".into());
                }
                admission.receive_fence(&ack, caller, now)?;
                R::Acknowledged
            }
            C::Complete(completion) => {
                admission.complete(&completion, caller, now)?;
                R::Acknowledged
            }
            C::PendingHostFences => {
                self.native_host(caller)?;
                R::HostFences(ledger.pending_host_fences(caller.device_public_key))
            }
            C::Snapshot { agent } => {
                self.native_host(caller)?;
                if let Some(agent) = agent {
                    self.peer
                        .workspace()
                        .check_task_execution_actor(&agent, None, now)?;
                }
                let mut public = tilo_peer_state::workspace_task::snapshot(
                    self.peer.admitted_room().ok_or("native task room absent")?,
                )?;
                for task in &mut public.tasks {
                    if let Some(resource) = &mut task.resource {
                        resource.agent_request = None;
                    }
                }
                for control in public.controls.values_mut() {
                    if let Some(held) = &mut control.confirmation {
                        *held = tilo_joined_room::PrivateTaskConfirmationStore::public_confirmation(
                            held,
                        );
                    }
                }
                R::Snapshot(
                    serde_json::to_string(&(public.tasks, public.controls, public.grants))
                        .map_err(|e| e.to_string())?,
                )
            }
            C::State { task, epoch } => {
                self.native_host(caller)?;
                R::State(ledger.state(&task, epoch))
            }
            // Stop is accepted only with its canonical person command, never as
            // a host's claimed person approval or a second authorization path.
            _ => return Err("native custody command family refused".into()),
        };
        self.persist_native_admission(&ledger)?;
        Ok(reply)
    }
}

fn coordination_command(
    room: &GovernedRoom,
    caller: &DeviceCert,
    command: C,
) -> Result<(uuid::Uuid, A, T), String> {
    let coordination = match command {
        C::PrivateCoordination {
            invocation,
            command,
        } => {
            if !room.peer.certificates().contains(caller)
                || room
                    .peer
                    .permissions(caller.device_public_key, SystemTime::now())?
                    .delegate
                    .is_some()
            {
                return Err("native task coordination requires the authenticated person".into());
            }
            (
                invocation,
                A::Person {
                    caller: caller.clone(),
                },
                command,
            )
        }
        C::Report {
            invocation,
            agent,
            command,
        } => {
            room.native_host(caller)?;
            let authority = match (&agent, &command) {
                (
                    Some(agent),
                    T::AppliedAck { .. }
                    | T::ExecutionPhase { .. }
                    | T::PublicProgress { .. }
                    | T::Presentation { .. }
                    | T::RequestConfirmation { .. }
                    | T::SettleConfirmation { .. }
                    | T::Finish { .. },
                ) => A::Agent {
                    attestor: caller.clone(),
                    agent_id: agent.clone(),
                },
                (None, T::Abandon { .. }) => A::Host {
                    attestor: caller.clone(),
                },
                _ => return Err("native report family refused".into()),
            };
            (invocation, authority, command)
        }
        _ => return Err("native coordination command family refused".into()),
    };
    Ok(coordination)
}

impl Rooms {
    pub(super) async fn native_task_private(
        &self,
        room: &mut GovernedRoom,
        caller: &DeviceCert,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        use tilo_joined_room::task_admission::{
            CommitOperation, OwnerCommitReceipt, OwnerCommitRequest,
        };
        let command = tilo_joined_room::task_admission::decode_admission_command(payload)?;
        let (command, model_start) = match command {
            C::PersonModelStart {
                invocation,
                command,
                snapshot,
            } => (
                C::PrivateCoordination {
                    invocation,
                    command,
                },
                Some(snapshot),
            ),
            command => (command, None),
        };
        let (invocation, authority, task_command) = match command {
            command @ (C::PrivateCoordination { .. } | C::Report { .. }) => {
                coordination_command(room, caller, command)?
            }
            command => {
                let reply = room.custody(caller, command, &self.inner.host.key)?;
                return tilo_joined_room::task_admission::encode_admission_reply(&reply);
            }
        };
        let request = OwnerCommitRequest {
            room: room.room().into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation,
                authority: authority.clone(),
                command: task_command.clone(),
            },
        };
        let binding = request.binding()?;
        let now = SystemTime::now();
        // Use the same private-text preparation as the desktop owner. This
        // retains hierarchy and strips the body before any CRDT history exists.
        let path = room.path.with_extension("private-task-text-v1.json");
        let texts = super::private_tasks::read_private(&path)?.unwrap_or_default();
        let mut text = tilo_joined_room::PrivateTaskTextStore::from_instructions(texts);
        let (report_command, report) = room.prepare_task_report(
            &task_command,
            &authority,
            invocation,
            binding.request_digest,
        )?;
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            room.peer
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
        let mut confirmations = room.task_confirmations(&snapshot)?;
        if let T::RequestConfirmation {
            task_id,
            run_epoch,
            confirmation,
        } = &report_command
        {
            room.load_native_admission()?
                .validate_confirmation_request(task_id, *run_epoch, confirmation)?;
        }
        let (confirmation_command, confirmation_write) =
            confirmations.prepare(&snapshot, &report_command)?;
        let (public, write) = text.prepare_private_command(&confirmation_command, &room.peer)?;
        let mut candidate = room.peer.clone();
        let (changed, result, approved_model) = if let Some(snapshot) = model_start {
            if serde_json::to_vec(&public).map_err(|e| e.to_string())?
                != serde_json::to_vec(&task_command).map_err(|e| e.to_string())?
            {
                return Err("signed MODEL approval command changed during preparation".into());
            }
            let (changed, result, model) = snapshot.commit(
                &mut candidate,
                request.clone(),
                tilo_joined_room::task_admission::model::NativeModelGovernorCustody {
                    host: caller,
                    key: &self.inner.host.key,
                    now: SystemTime::now(),
                },
            )?;
            (changed, result, Some(model))
        } else {
            let (changed, result) = candidate.workspace_mut().apply_workspace_task_command(
                invocation,
                binding.request_digest,
                &authority,
                &public,
                now,
            )?;
            (changed, result, None)
        };
        if changed && let Some(model) = approved_model {
            let T::ApproveRun { task_id, .. } = &task_command else {
                return Err("MODEL approval task absent".into());
            };
            let state = tilo_peer_state::workspace_task::snapshot(
                candidate
                    .admitted_room()
                    .ok_or("MODEL approval room absent")?,
            )?;
            let mut full = state
                .controls
                .get(task_id)
                .and_then(|control| control.model_authority.clone())
                .ok_or("MODEL approved authority absent")?;
            full.contract = model.contract;
            let path = room
                .path
                .with_extension("private-task-model-authority-v1.json");
            let mut models: std::collections::BTreeMap<
                String,
                tilo_desktop_model::task::WorkspaceTaskModelAuthority,
            > = super::private_tasks::read_private(&path)?.unwrap_or_default();
            models.insert(task_id.clone(), full);
            let bytes = serde_json::to_vec(&models).map_err(|e| e.to_string())?;
            if models.len() > 4096 || bytes.len() > 16 * 1024 * 1024 {
                return Err("private model snapshot capacity exhausted".into());
            }
            tilo_files::native::atomic::write_private(&path, &bytes)
                .map_err(|_| "private model snapshot persistence failed")?;
        }
        if changed {
            let accepted = tilo_peer_state::workspace_task::snapshot(
                candidate
                    .admitted_room()
                    .ok_or("private task room absent")?,
            )?;
            confirmations.retain(&accepted, confirmation_write)?;
            room.persist_task_confirmations(&confirmations)?;
        }
        if changed && let Some(report) = report {
            // The private write-ahead record must reach disk before commit can
            // publish its neutral receipt. Retries reuse its timestamp and actor.
            room.retain_task_report(report, binding.request_digest)?;
        }
        if changed && let Some(write) = write {
            if let Some(reference) = write.instruction_reference() {
                candidate
                    .workspace_mut()
                    .bind_workspace_task_private_instruction(&result, &reference)?;
            }
            text.retain_private_instruction(&result, write)?;
            let bytes = serde_json::to_vec(text.instructions())
                .map_err(|_| "private task encoding failed")?;
            if bytes.len() > 72 * 1024 * 1024 {
                return Err("private task custody exceeds capacity".into());
            }
            tilo_files::native::atomic::write_private(&path, &bytes)
                .map_err(|_| "private task persistence failed")?;
        }
        if let T::CancelForRun {
            task_id, run_epoch, ..
        } = &task_command
        {
            // Close admission durably before publishing cancellation. A failed
            // publication leaves the safer closed ledger, never reopens it.
            let mut ledger = room.load_native_admission()?;
            let id = tilo_desktop_model::RoomId::parse(room.room()).ok_or("invalid native room")?;
            NativeGovernorAdmission {
                room: id,
                peer: &room.peer,
                ledger: &mut ledger,
                continuity: room.record.generation,
                key: &self.inner.host.key,
            }
            .stop(task_id, *run_epoch)?;
            room.persist_native_admission(&ledger)?;
        }
        if changed {
            let collaboration = room.record.collaboration.clone();
            room.commit(candidate, &self.inner.host.key, now, |_, _| {
                Ok(collaboration)
            })?;
        }
        // A retry also cleans up a crash after commit but before compaction.
        room.compact_task_confirmations()?;
        self.settle(room).await?;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: room.record.generation,
            // Hash the exact committed durable bytes, not multipart descriptors.
            aggregate_hash: *blake3::hash(&room.parts.load(&room.record.peer)?).as_bytes(),
        };
        tilo_joined_room::task_admission::encode_admission_reply(&R::Coordination(receipt))
    }
}
