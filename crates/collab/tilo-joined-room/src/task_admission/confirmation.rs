//! Request-time confirmation custody. Approval can bind one child and its required
//! deterministic activation, never another child or a changed contract.
use super::*;
use crate::task_policy::NativeTaskActionRequirement;
use tilo_peer_state::workspace_task::WorkspaceTaskSnapshot;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct FrozenConfirmation {
    requirement: NativeTaskActionRequirement,
    host: [u8; 32],
    consumed: Option<uuid::Uuid>,
}

pub(super) fn activation_id(child: uuid::Uuid) -> uuid::Uuid {
    let digest = blake3::hash(format!("native-host-activation:{child}").as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    uuid::Uuid::from_bytes(bytes)
}

pub(super) fn same_request(a: &NativeInvocationBinding, b: &NativeInvocationBinding) -> bool {
    a.request == b.request
        && a.room == b.room
        && a.task == b.task
        && a.agent == b.agent
        && a.epoch == b.epoch
        && a.revision == b.revision
        && a.host == b.host
        && a.app == b.app
        && a.provider == b.provider
        && a.arguments_digest == b.arguments_digest
        && a.private_run_digest == b.private_run_digest
        && ((a.invocation == a.request
            && b.invocation == activation_id(a.request)
            && b.action == "open"
            && b.resource == format!("app.{}", b.app))
            || (b.invocation == b.request
                && a.invocation == activation_id(b.request)
                && a.action == "open"
                && a.resource == format!("app.{}", a.app)))
}

pub(super) fn exact(
    requirement: &NativeTaskActionRequirement,
    room: RoomId,
    host: [u8; 32],
) -> Result<NativeInvocationBinding, String> {
    let run = requirement
        .binding
        .as_ref()
        .ok_or("native task binding absent")?;
    let invocation = requirement
        .invocation
        .as_ref()
        .ok_or("native exact invocation absent")?;
    Ok(NativeInvocationBinding {
        room,
        task: run.task_id.clone(),
        agent: run.agent_id.clone(),
        epoch: run.epoch,
        revision: run.revision,
        invocation: invocation.id,
        request: invocation.request,
        host,
        app: requirement.app.clone(),
        provider: invocation.provider.clone(),
        resource: requirement.resource.clone(),
        action: requirement.action.clone(),
        arguments_digest: invocation.arguments_digest,
        contract_digest: invocation.contract_digest,
        ceiling_digest: requirement.ceiling_digest()?,
        private_run_digest: invocation.private_run_digest,
    })
}

impl TaskAdmissionLedger {
    pub(super) fn validate_confirmations(&self) -> Result<(), String> {
        if self.confirmations.len() > MAX_RESERVATIONS {
            return Err("native confirmation capacity exhausted".into());
        }
        for (asking, frozen) in &self.confirmations {
            let id: uuid::Uuid = asking
                .parse()
                .map_err(|_| "native retained asking UUID invalid")?;
            let invocation = frozen
                .requirement
                .invocation
                .as_ref()
                .ok_or("native retained review invocation absent")?;
            if id.is_nil()
                || id.to_string() != *asking
                || invocation.id.is_nil()
                || invocation.id != invocation.request
                || invocation.activation_for.is_some()
                || frozen.requirement.confirmation.is_some()
                || frozen.consumed.is_some_and(|child| child != invocation.id)
                || serde_json::to_vec(&frozen.requirement)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 512 * 1024
            {
                return Err("native retained review custody invalid".into());
            }
            frozen.requirement.confirmation_ceiling_digest()?;
        }
        Ok(())
    }
    /// Freeze before publishing the review card. This is not dispatch authority.
    /// # Errors
    /// Stale policy, changed asking identity, or invalid request metadata.
    pub fn freeze_confirmation(
        &mut self,
        peer: &crate::StatePeer,
        snapshot: &WorkspaceTaskSnapshot,
        asking: uuid::Uuid,
        requirement: &NativeTaskActionRequirement,
        host: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        if asking.is_nil() || requirement.confirmation.is_some() {
            return Err("native confirmation asking identity invalid".into());
        }
        let room = RoomId::parse(peer.room()).ok_or("native room identity invalid")?;
        let binding = exact(requirement, room, host.device_public_key)?;
        let invocation = requirement
            .invocation
            .as_ref()
            .ok_or("native invocation absent")?;
        if invocation.activation_for.is_some()
            || invocation.id != invocation.request
            || self.is_fenced(&binding.task, binding.epoch)
        {
            return Err("native review must name one unfenced child request".into());
        }
        match requirement.check_invocation_with_snapshot(peer, snapshot, &binding, now) {
            Ok(()) => {}
            Err(error) if NativeTaskActionRequirement::awaits_confirmation(&error) => {}
            Err(error) => return Err(error),
        }
        let key = asking.to_string();
        if let Some(frozen) = self.confirmations.get(&key) {
            if frozen.host != host.device_public_key
                || serde_json::to_vec(&frozen.requirement).map_err(|e| e.to_string())?
                    != serde_json::to_vec(requirement).map_err(|e| e.to_string())?
            {
                return Err("native review asking UUID already bound".into());
            }
            return Ok(());
        }
        if snapshot.controls.values().any(|control| {
            control
                .confirmation
                .as_ref()
                .is_some_and(|held| held.asking == asking)
        }) {
            return Err(
                "native review cannot freeze a contract after publication or answer".into(),
            );
        }
        if self.confirmations.len() >= MAX_RESERVATIONS {
            return Err("native confirmation capacity exhausted".into());
        }
        self.confirmations.insert(
            key,
            FrozenConfirmation {
                requirement: requirement.clone(),
                host: host.device_public_key,
                consumed: None,
            },
        );
        Ok(())
    }

    /// Original request identity, not a new grant or an activation identity.
    /// # Errors
    /// The current held review or exact run no longer matches request-time custody.
    pub fn frozen_confirmation_invocation(
        &self,
        snapshot: &WorkspaceTaskSnapshot,
        binding: &crate::task_policy::NativeTaskBinding,
        asking: uuid::Uuid,
    ) -> Result<uuid::Uuid, String> {
        let held = snapshot
            .controls
            .get(&binding.task_id)
            .and_then(|c| c.confirmation.as_ref())
            .ok_or("native held review absent")?;
        let frozen = self
            .confirmations
            .get(&asking.to_string())
            .ok_or("native request-time review custody absent")?;
        let original = frozen
            .requirement
            .binding
            .as_ref()
            .ok_or("native frozen run absent")?;
        if held.asking != asking
            || held.run_epoch != binding.epoch
            || held.agent_id != binding.agent_id
            || serde_json::to_vec(original).map_err(|e| e.to_string())?
                != serde_json::to_vec(binding).map_err(|e| e.to_string())?
        {
            return Err("native held review run changed".into());
        }
        Ok(frozen
            .requirement
            .invocation
            .as_ref()
            .ok_or("native frozen invocation absent")?
            .id)
    }

    /// Reject review cards that were not frozen before publication.
    /// # Errors
    /// No matching original request/contract/argument custody exists.
    pub fn validate_confirmation_request(
        &self,
        task: &str,
        epoch: u64,
        held: &tilo_desktop_model::WorkspaceTaskConfirmation,
    ) -> Result<(), String> {
        let frozen = self
            .confirmations
            .get(&held.asking.to_string())
            .ok_or("native review has no frozen request custody")?;
        let requirement = &frozen.requirement;
        let run = requirement
            .binding
            .as_ref()
            .ok_or("native frozen run absent")?;
        let invocation = requirement
            .invocation
            .as_ref()
            .ok_or("native frozen invocation absent")?;
        let argument = invocation.arguments.as_deref().unwrap_or("{}");
        let argument = argument
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .unwrap_or(argument);
        let arguments: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(argument).map_err(|_| "native frozen review arguments invalid")?;
        if run.task_id != task
            || run.epoch != epoch
            || held.run_epoch != epoch
            || held.agent_id != run.agent_id
            || held.app != requirement.app
            || held.resource != requirement.resource
            || held.action != requirement.action
            || held.resource_kind != requirement.resource_kind
            || held.kind != requirement.need.kind
            || held.risk != invocation.contract.risk
            || held.arguments_digest != tilo_actions::draft::arguments_digest(&arguments)
        {
            return Err("native review differs from frozen original request".into());
        }
        Ok(())
    }

    /// Resolve a person answer and its request-time ceiling, never a wire waiver.
    /// # Errors
    /// Missing frozen custody, changed exact metadata, or denied current policy.
    pub fn check_action(
        &self,
        peer: &crate::StatePeer,
        snapshot: &WorkspaceTaskSnapshot,
        requirement: &NativeTaskActionRequirement,
        binding: &NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        let approved =
            requirement.confirmation_binding_with_snapshot(peer, snapshot, binding, now)?;
        let Some((reference, child)) = approved else {
            return Ok(());
        };
        let reviewed = requirement
            .invocation
            .as_ref()
            .and_then(|i| i.activation_for.as_deref())
            .unwrap_or(requirement);
        let frozen = self
            .confirmations
            .get(&reference)
            .ok_or("native confirmation has no request-time custody")?;
        let original = frozen
            .requirement
            .invocation
            .as_ref()
            .ok_or("native frozen invocation absent")?;
        if original.id != child
            || original.request != child
            || binding.request != child
            || frozen.host != binding.host
            || frozen.requirement.confirmation_ceiling_digest()?
                != reviewed.confirmation_ceiling_digest()?
            || frozen.consumed.is_some_and(|used| used != child)
        {
            return Err("native person answer binding changed or was already consumed".into());
        }
        Ok(())
    }

    pub(super) fn consume_confirmation(
        &mut self,
        binding: &NativeInvocationBinding,
    ) -> Result<(), String> {
        for frozen in self.confirmations.values_mut() {
            let invocation = frozen
                .requirement
                .invocation
                .as_ref()
                .ok_or("native frozen invocation absent")?;
            if invocation.id == binding.request {
                if frozen
                    .consumed
                    .is_some_and(|child| child != binding.request)
                {
                    return Err("native confirmation already consumed by another child".into());
                }
                frozen.consumed = Some(binding.request);
            }
        }
        Ok(())
    }
}

impl NativeCommittedView {
    /// Exact original host dispatch provenance for reads inside an existing owner turn.
    /// # Errors
    /// Missing, uncertain-before-dispatch, or foreign-host custody.
    pub fn retained_native_invocation(
        &self,
        invocation: uuid::Uuid,
    ) -> Result<&NativeInvocationBinding, String> {
        let entry = self
            .record
            .task_admission
            .reservations
            .get(&invocation)
            .ok_or("native retained invocation absent")?;
        if !entry.host_started
            || entry.permit.is_none()
            || entry.custody != Custody::Dispatched
            || entry.reservation.binding.host != self.device().device_public_key
        {
            return Err("native retained host dispatch provenance absent".into());
        }
        Ok(&entry.reservation.binding)
    }

    /// Current action ceiling and exact private owner approval.
    /// # Errors
    /// Frozen custody or current authority differs.
    pub fn check_native_task_action(
        &self,
        requirement: &NativeTaskActionRequirement,
        binding: &NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.device() != self.governor() {
            // Host dispatch has already verified the exact governor-signed permit.
            // Private answer bodies stay at the governor; a host rechecks every
            // live Deny without treating a caller reference as authority.
            return requirement.check_dispatch_ceiling(self.peer(), binding, now);
        }
        self.check_private_run(requirement)?;
        self.record.task_admission.check_action(
            self.peer(),
            &self.task_policy_snapshot()?,
            requirement,
            binding,
            now,
        )
    }
    /// Policy-only preflight; it cannot admit dispatch.
    /// # Errors
    /// Policy denies or requires a person answer.
    pub fn check_native_task_preflight(
        &self,
        requirement: &NativeTaskActionRequirement,
        agent: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        self.check_private_run(requirement)?;
        requirement.check_with_snapshot(
            self.peer(),
            &self.task_policy_snapshot()?,
            &self.record.room.to_string(),
            agent,
            now,
        )
    }
    /// Read-only policy recheck of ledger-authenticated original provenance.
    /// # Errors
    /// Exact provenance changed or current policy denies.
    pub fn check_retained_native_task_action(
        &self,
        requirement: &NativeTaskActionRequirement,
        binding: &NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        requirement.check_retained(self.peer(), binding, now)
    }
    fn check_private_run(&self, requirement: &NativeTaskActionRequirement) -> Result<(), String> {
        let binding = requirement
            .binding
            .as_ref()
            .ok_or("native run binding absent")?;
        let invocation = requirement
            .invocation
            .as_ref()
            .ok_or("native invocation absent")?;
        if invocation.private_run_digest
            != NativeWorkspaceOwner::current_private_run_digest(self, binding)?
        {
            return Err("native private run snapshot changed".into());
        }
        Ok(())
    }
}

impl NativeWorkspaceOwner {
    /// Recover the original reviewed request from the authenticated private run.
    /// # Errors
    /// Actor authorization, held review, or owner generation changed.
    pub fn workspace_task_confirmation_invocation(
        &self,
        room: RoomId,
        binding: &crate::task_policy::NativeTaskBinding,
        asking: uuid::Uuid,
        authority: &tilo_peer_state::workspace_task::WorkspaceTaskAuthority,
        now: SystemTime,
    ) -> Result<uuid::Uuid, String> {
        let generation = self.with_current(room, |view| Ok(view.generation()))?;
        self.workspace_task_private_confirmation(
            room,
            &binding.task_id,
            binding.epoch,
            authority,
            now,
        )?;
        self.with_current(room, |view| {
            if view.generation() != generation {
                return Err("native review authorization changed; retry".into());
            }
            view.record.task_admission.frozen_confirmation_invocation(
                &view.task_policy_snapshot()?,
                binding,
                asking,
            )
        })
    }
    /// Persist request-time review custody before exposing a person-facing draft.
    /// # Errors
    /// Invalid host, stale private run, changed request, or persistence failure.
    pub fn freeze_native_confirmation(
        &self,
        room: RoomId,
        asking: uuid::Uuid,
        requirement: &NativeTaskActionRequirement,
        host: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        governor_custody(&candidate, key)?;
        host_evidence(&candidate, host, now)?;
        candidate.check_private_run(requirement)?;
        let snapshot = candidate.task_policy_snapshot()?;
        candidate.record.task_admission.freeze_confirmation(
            &candidate.peer,
            &snapshot,
            asking,
            requirement,
            host,
            now,
        )?;
        self.publish_task_admission(room, candidate)
    }
    /// Observe original durable dispatch provenance without reserving or consuming.
    /// # Errors
    /// Missing dispatch intent, wrong host, or a current read-policy denial.
    pub fn authorize_retained_native_invocation<T>(
        &self,
        room: RoomId,
        invocation: uuid::Uuid,
        read: impl FnOnce(
            &NativeCommittedView,
            &NativeInvocationBinding,
            SystemTime,
        ) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_current(room, |view| {
            let entry = view
                .record
                .task_admission
                .reservations
                .get(&invocation)
                .ok_or("native retained invocation absent")?;
            if !entry.host_started
                || entry.permit.is_none()
                || entry.custody != Custody::Dispatched
                || entry.reservation.binding.host != view.device().device_public_key
            {
                return Err("native retained host dispatch provenance absent".into());
            }
            read(view, &entry.reservation.binding, SystemTime::now())
        })
    }
}
