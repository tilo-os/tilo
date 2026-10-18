//! Signed physical-host snapshots for a separately authenticated person start.
//! This is not a bearer approval: the governor must receive the exact canonical
//! `ApproveRun` request over the approving person's authenticated private channel.
use super::model::{NativeModelContract, NativeModelStartAuthority};
use super::{CommitOperation, OwnerCommitRequest};
use crate::{NativeWorkspaceOwner, RoomId, StatePeer};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskModelStart,
};
use tilo_user_identity::DeviceCert;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelPersonStartSnapshot {
    room: RoomId,
    host: [u8; 32],
    request_digest: [u8; 32],
    invocation: uuid::Uuid,
    expires_at: u64,
    contract: NativeModelContract,
    signature: [[u8; 32]; 2],
}
impl std::fmt::Debug for NativeModelPersonStartSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeModelPersonStartSnapshot { private: redacted }")
    }
}

fn person_start(request: &OwnerCommitRequest) -> Result<(&DeviceCert, &str, uuid::Uuid), String> {
    let CommitOperation::WorkspaceTaskCommand {
        invocation,
        authority: WorkspaceTaskAuthority::Person { caller },
        command: WorkspaceTaskCommand::ApproveRun { task_id, .. },
    } = &request.operation
    else {
        return Err("model snapshot requires a canonical person approval".into());
    };
    if invocation.is_nil() {
        return Err("model approval invocation absent".into());
    }
    Ok((caller, task_id, *invocation))
}

struct SignedStart<'a> {
    snapshot: &'a NativeModelPersonStartSnapshot,
    request: &'a OwnerCommitRequest,
    caller: &'a DeviceCert,
}
impl NativeModelStartAuthority for SignedStart<'_> {
    fn approved_model_start(
        &self,
        view: &crate::NativeCommittedView,
        task_id: &str,
        person: &DeviceCert,
        now: SystemTime,
    ) -> Result<NativeModelContract, String> {
        if person != self.caller || person_start(self.request)?.1 != task_id {
            return Err("signed model approval person or task changed".into());
        }
        self.snapshot
            .verify_for_start(view.peer(), self.caller, self.request, now)?;
        Ok(self.snapshot.contract.clone())
    }
}

/// A caller-held durable governor turn. Implementations commit only after
/// verifying the snapshot against their current canonical peer.
pub trait NativeModelPersonStartTarget {
    type Receipt;
    /// # Errors
    /// Rejects invalid approval snapshots, current authority or durable custody.
    fn commit_person_start(
        self,
        snapshot: &NativeModelPersonStartSnapshot,
        request: OwnerCommitRequest,
        custody: super::model::NativeModelGovernorCustody<'_>,
    ) -> Result<Self::Receipt, String>;
}

impl NativeModelPersonStartTarget for &NativeWorkspaceOwner {
    type Receipt = super::OwnerCommitReceipt;
    fn commit_person_start(
        self,
        snapshot: &NativeModelPersonStartSnapshot,
        request: OwnerCommitRequest,
        custody: super::model::NativeModelGovernorCustody<'_>,
    ) -> Result<Self::Receipt, String> {
        snapshot.commit_owner(self, &request, custody)
    }
}

impl NativeModelPersonStartTarget for &mut StatePeer {
    type Receipt = (
        bool,
        tilo_desktop_model::WorkspaceTaskMutationResult,
        WorkspaceTaskModelStart,
    );
    fn commit_person_start(
        self,
        snapshot: &NativeModelPersonStartSnapshot,
        request: OwnerCommitRequest,
        custody: super::model::NativeModelGovernorCustody<'_>,
    ) -> Result<Self::Receipt, String> {
        if custody.key.verifying_key().to_bytes() != self.authority().device_public_key {
            return Err("model approval requires current governor writer custody".into());
        }
        snapshot.commit_to_peer(self, custody.host, &request, custody.now)
    }
}

impl NativeModelPersonStartSnapshot {
    /// Commit through either the native owner or a caller-held governed-room
    /// writer turn. Transport authentication supplies `custody.host` in both cases.
    /// # Errors
    /// Rejects an invalid approval snapshot, changed authority or failed durable commit.
    pub fn commit<T: NativeModelPersonStartTarget>(
        &self,
        target: T,
        request: OwnerCommitRequest,
        custody: super::model::NativeModelGovernorCustody<'_>,
    ) -> Result<T::Receipt, String> {
        target.commit_person_start(self, request, custody)
    }

    /// Governor entry point for a separately transport-authenticated person.
    /// `custody.host` is the authenticated physical approving device, while
    /// `custody.key` is the governor writer's signing key. Verification runs
    /// inside the same durable owner turn as the person approval reducer.
    fn commit_owner(
        &self,
        owner: &NativeWorkspaceOwner,
        request: &OwnerCommitRequest,
        custody: super::model::NativeModelGovernorCustody<'_>,
    ) -> Result<super::OwnerCommitReceipt, String> {
        // Check the command family even if the reducer would never call a
        // model hook (for example, an attempt to reuse this as a host start).
        if person_start(request)?.0 != custody.host {
            return Err("signed model approval requires its authenticated person".into());
        }
        let room = RoomId::parse(&request.room).ok_or("model approval room invalid")?;
        owner.with_current(room, |view| {
            self.verify_for_start(view.peer(), custody.host, request, custody.now)
                .map(|_| ())
        })?;
        let hook = SignedStart {
            snapshot: self,
            request,
            caller: custody.host,
        };
        owner.commit_workspace_task_command_with_model_start(
            request.clone(),
            custody.key,
            &hook,
            custody.now,
        )
    }

    /// Governed-room writer entry point. The caller holds the durable room turn
    /// and must persist the full returned contract privately before publishing
    /// the scrubbed peer. No governor-local Settings hook is consulted.
    fn commit_to_peer(
        &self,
        peer: &mut StatePeer,
        caller: &DeviceCert,
        request: &OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<
        (
            bool,
            tilo_desktop_model::WorkspaceTaskMutationResult,
            WorkspaceTaskModelStart,
        ),
        String,
    > {
        let model = self.verify_for_start(peer, caller, request, now)?;
        let CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority,
            command,
        } = &request.operation
        else {
            return Err("model snapshot requires a person task command".into());
        };
        let (changed, result) = peer
            .workspace_mut()
            .apply_workspace_task_command_with_model_start(
                *invocation,
                request.binding()?.request_digest,
                authority,
                command,
                now,
                Some(&model),
            )?;
        Ok((changed, result, model))
    }

    /// Called by the trusted shell when a person consciously approves a run,
    /// never from a gateway claim or application-provided model selection.
    /// The physical host must itself be the authenticated approving device.
    /// # Errors
    /// Rejects an invalid approval snapshot, changed authority or failed durable commit.
    pub fn capture(
        owner: &NativeWorkspaceOwner,
        request: &OwnerCommitRequest,
        hook: &dyn NativeModelStartAuthority,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Self, String> {
        let room = RoomId::parse(&request.room).ok_or("model approval room invalid")?;
        let (person, task, invocation) = person_start(request)?;
        owner.with_current(room, |view| {
            if view.device() != person || key.verifying_key().to_bytes() != person.device_public_key
            {
                return Err("model approval requires physical approving device custody".into());
            }
            super::host_evidence(view, person, now)?;
            let contract = hook.approved_model_start(view, task, person, now)?;
            contract.source_for(&request.room, task)?;
            let mut snapshot = Self {
                room,
                host: person.device_public_key,
                request_digest: request.binding()?.request_digest,
                invocation,
                expires_at: super::super::enrollment::ns(
                    now.checked_add(Duration::from_secs(120))
                        .ok_or("model approval expiry overflow")?,
                )?,
                contract,
                signature: [[0; 32]; 2],
            };
            let signature = super::sign(
                "tilo-native-model-person-start-v1",
                &snapshot.payload(),
                key,
            )?;
            snapshot.signature = signature;
            Ok(snapshot)
        })
    }

    fn payload(&self) -> impl Serialize + '_ {
        (
            self.room,
            self.host,
            self.request_digest,
            self.invocation,
            self.expires_at,
            &self.contract,
        )
    }

    /// Governor-side verification before passing the returned snapshot into the
    /// canonical reducer in the same durable transaction. `caller` must come
    /// from authenticated transport, not a field in a decoded command.
    /// # Errors
    /// Rejects an invalid approval snapshot, changed authority or failed durable commit.
    pub fn verify_for_start(
        &self,
        peer: &StatePeer,
        caller: &DeviceCert,
        request: &OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<WorkspaceTaskModelStart, String> {
        let (person, task, invocation) = person_start(request)?;
        let now_ns = super::super::enrollment::ns(now)?;
        if person != caller
            || !peer.certificates().contains(caller)
            || peer.room() != self.room.to_string()
            || request.room != self.room.to_string()
            || self.host != caller.device_public_key
            || self.invocation != invocation
            || self.request_digest != request.binding()?.request_digest
            || self.expires_at <= now_ns
            || self.expires_at.saturating_sub(now_ns) > 120_000_000_000
            || (!caller.capabilities.can_act_as_host && caller != peer.authority())
            || caller.user_public_key != peer.authority().user_public_key
        {
            return Err("model person approval binding changed or expired".into());
        }
        super::verify(
            "tilo-native-model-person-start-v1",
            &self.payload(),
            self.host,
            self.signature,
        )?;
        let member = peer.permissions(self.host, now)?;
        if member.delegate.is_some()
            || !member.allows(&self.contract.request_model, self.contract.kind, now)
        {
            return Err("model approval requires current person model permission".into());
        }
        let state = tilo_peer_state::workspace_task::snapshot(
            peer.admitted_room().ok_or("model approval room absent")?,
        )?;
        if !state.tasks.iter().any(|current| {
            current.id == task && current.route == tilo_desktop_model::WorkspaceTaskRoute::Model
        }) {
            return Err("model approval requires its current model task".into());
        }
        self.contract.source_for(&request.room, task)?;
        let selection = &self.contract.selection;
        if selection.app.is_empty()
            || selection.provider.is_empty()
            || selection.model.is_empty()
            || !selection.contract.is_object()
        {
            return Err("model approval snapshot incomplete".into());
        }
        Ok(WorkspaceTaskModelStart {
            request_model: self.contract.request_model.clone(),
            kind: self.contract.kind,
            contract: serde_json::to_value(&self.contract).map_err(|e| e.to_string())?,
        })
    }
}
