//! Exact native invocation custody. This ledger is owner storage, never room CRDT data.
//!
//! A Stop closes governor admission before contacting hosts. An offline host keeps
//! the barrier incomplete; neither model acknowledgements nor elapsed wall time
//! constitute evidence that a host has fenced its admission queue.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
mod confirmation;
pub mod model;
pub mod model_delivery;
pub mod model_host;
pub mod model_start;
pub mod private_run;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;
use tilo_user_identity::DeviceCert;
// The private governor and native owner use precisely the same command binding.
pub use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};

#[cfg(test)]
#[path = "task_admission_tests.rs"]
mod tests;

const MAX_RESERVATIONS: usize = 4096;

/// Private point-to-point governor vocabulary. Append variants only.
#[derive(Clone, Serialize, Deserialize)]
pub enum NativeAdmissionCommand {
    Reserve {
        binding: NativeInvocationBinding,
        requirement: crate::task_policy::NativeTaskActionRequirement,
    },
    Claim {
        reservation: NativeInvocationReservation,
        requirement: crate::task_policy::NativeTaskActionRequirement,
    },
    Stop {
        task: String,
        epoch: u64,
    },
    FenceAcknowledgement(NativeHostFenceAcknowledgement),
    State {
        task: String,
        epoch: u64,
    },
    /// Host-only pending queue barriers, delivered privately.
    PendingHostFences,
    Complete(NativeInvocationCompletion),
    PrivateCoordination {
        invocation: uuid::Uuid,
        command: tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    },
    Report {
        invocation: uuid::Uuid,
        agent: Option<String>,
        command: tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    },
    FreezeConfirmation {
        asking: uuid::Uuid,
        requirement: crate::task_policy::NativeTaskActionRequirement,
    },
    CheckAction {
        requirement: crate::task_policy::NativeTaskActionRequirement,
    },
    Snapshot {
        agent: Option<String>,
    },
    /// Separate physical-host enrollment; never inferred from ReserveModel.
    RegisterModelHost(model_host::NativeModelHostRegistration),
    /// Signed successful physical-host HTTP delivery, never an actor report.
    ModelDelivery(model_delivery::NativeModelDeliveryReceipt),
    /// Closes the whole run and returns its durable Stop fence.
    RevokeModelHost {
        registration: uuid::Uuid,
    },
    ReserveModel {
        registration: uuid::Uuid,
        requirement: model::TypedNativeModelRequirement,
    },
    ClaimModel {
        registration: uuid::Uuid,
        requirement: model::TypedNativeModelRequirement,
        reservation: NativeInvocationReservation,
    },
    /// Signed physical-device selection, bound to a separately authenticated person command.
    PersonModelStart {
        invocation: uuid::Uuid,
        command: tilo_peer_state::workspace_task::WorkspaceTaskCommand,
        snapshot: model_start::NativeModelPersonStartSnapshot,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NativeAdmissionReply {
    Reserved(NativeInvocationReservation),
    Claimed(NativeInvocationPermit),
    Fence(NativeTaskFence),
    Acknowledged,
    State(NativeTaskAdmissionState),
    HostFences(Vec<NativeTaskFence>),
    TaskReported { generation: u64 },
    Coordination(OwnerCommitReceipt),
    Policy(tilo_action_policy::Answer),
    Snapshot(String),
}

/// Encode private custody with an explicit inner version. JSON is intentional:
/// published contracts use omitted optional fields and cannot be postcard structs.
/// # Errors
/// Returns an error if command serialization fails.
pub fn encode_admission_command(command: &NativeAdmissionCommand) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&(1u8, command)).map_err(|error| error.to_string())
}
/// # Errors
/// Returns an error for malformed JSON or an unsupported version.
pub fn decode_admission_command(bytes: &[u8]) -> Result<NativeAdmissionCommand, String> {
    let (version, command): (u8, NativeAdmissionCommand) =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if version != 1 {
        return Err("unsupported native custody command version".into());
    }
    Ok(command)
}
/// # Errors
/// Returns an error if reply serialization fails.
pub fn encode_admission_reply(reply: &NativeAdmissionReply) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&(1u8, reply)).map_err(|error| error.to_string())
}
/// # Errors
/// Returns an error for malformed JSON or an unsupported version.
pub fn decode_admission_reply(bytes: &[u8]) -> Result<NativeAdmissionReply, String> {
    let (version, reply): (u8, NativeAdmissionReply) =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if version != 1 {
        return Err("unsupported native custody reply version".into());
    }
    Ok(reply)
}
/// All effect-bearing metadata must be frozen before requesting custody.
/// The digests cover the canonical arguments, stable action contract and private
/// run envelope respectively; they are not bearer authority or reusable leases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInvocationBinding {
    pub room: RoomId,
    pub task: String,
    pub agent: String,
    pub epoch: u64,
    pub revision: u64,
    pub invocation: uuid::Uuid,
    pub request: uuid::Uuid,
    pub host: [u8; 32],
    pub app: String,
    pub provider: String,
    pub resource: String,
    pub action: String,
    pub arguments_digest: [u8; 32],
    pub contract_digest: [u8; 32],
    pub ceiling_digest: [u8; 32],
    pub private_run_digest: [u8; 32],
}

/// The typed MODEL binding has no application action address. This is only a
/// frontier discriminator, not authority: reserve and claim still run their
/// exact typed policy check before consulting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeInvocationFrontier {
    Model,
    Action,
}

impl NativeInvocationBinding {
    fn frontier(&self) -> Result<NativeInvocationFrontier, String> {
        match (self.resource.is_empty(), self.action.is_empty()) {
            (true, true) => Ok(NativeInvocationFrontier::Model),
            (_, false) => Ok(NativeInvocationFrontier::Action),
            (false, true) => Err("native invocation action address is incomplete".into()),
        }
    }

    fn matches_frontier(&self, desired: u64, applied: u64) -> Result<bool, String> {
        let frontier = self.frontier()?;
        // MODEL prepares the plan that will acknowledge the desired revision.
        // Application actions cannot run until that acknowledgement is durable.
        Ok(self.revision == desired
            && match frontier {
                NativeInvocationFrontier::Model => true,
                NativeInvocationFrontier::Action => self.revision == applied,
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInvocationReservation {
    pub binding: NativeInvocationBinding,
    pub governor: [u8; 32],
    pub continuity: u64,
    pub serial: u64,
    pub signature: [[u8; 32]; 2],
}

/// Signed terminal digest. Raw outputs and private runtime leases stay on the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInvocationCompletion {
    pub permit: NativeInvocationPermit,
    pub result_digest: [u8; 32],
    pub host: [u8; 32],
    pub signature: [[u8; 32]; 2],
}
impl NativeInvocationCompletion {
    fn verify(&self) -> Result<(), String> {
        self.permit.verify()?;
        verify(
            "tilo-native-completion-v1",
            &(&self.permit, self.result_digest, self.host),
            self.host,
            self.signature,
        )
    }
}
/// A governor transaction has rechecked scope and taken dispatch custody for
/// exactly this invocation. A reservation alone cannot execute an effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInvocationPermit {
    pub reservation: NativeInvocationReservation,
    pub continuity: u64,
    pub serial: u64,
    pub signature: [[u8; 32]; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTaskFence {
    pub room: RoomId,
    pub task: String,
    pub epoch: u64,
    pub governor: [u8; 32],
    pub continuity: u64,
    pub serial: u64,
    pub hosts: BTreeSet<[u8; 32]>,
    pub signature: [[u8; 32]; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeHostFenceAcknowledgement {
    pub fence: NativeTaskFence,
    pub host: [u8; 32],
    /// Effects already dispatched remain in flight, and must never be retried
    /// automatically. The fence does not undo non-cancellable effects.
    pub dispatched: BTreeSet<uuid::Uuid>,
    pub signature: [[u8; 32]; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeTaskAdmissionState {
    Open,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Custody {
    Reserved,
    Dispatched,
    Rejected,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    reservation: NativeInvocationReservation,
    permit: Option<NativeInvocationPermit>,
    host_started: bool,
    #[serde(default)]
    completion: Option<NativeInvocationCompletion>,
    custody: Custody,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskAdmissionLedger {
    serial: u64,
    model_deliveries: BTreeMap<uuid::Uuid, model_delivery::NativeModelDeliveryReceipt>,
    model_hosts: BTreeMap<uuid::Uuid, model_host::NativeModelHostRegistration>,
    confirmations: BTreeMap<String, confirmation::FrozenConfirmation>,
    reservations: BTreeMap<uuid::Uuid, Entry>,
    fences: BTreeMap<String, NativeTaskFence>,
    acknowledgements: BTreeMap<String, BTreeMap<String, NativeHostFenceAcknowledgement>>,
}

impl std::fmt::Debug for TaskAdmissionLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskAdmissionLedger { private: redacted }")
    }
}

impl TaskAdmissionLedger {
    /// Exact governor dispatch custody, not a caller-provided source label.
    ///
    /// # Errors
    /// Refuses absent or undispatched invocations and mismatched hosts.
    /// A fence does not erase provenance of an already admitted effect.
    pub fn dispatched_invocation(
        &self,
        id: uuid::Uuid,
        host: [u8; 32],
    ) -> Result<&NativeInvocationBinding, String> {
        let entry = self
            .reservations
            .get(&id)
            .ok_or("native invocation absent")?;
        let binding = &entry.reservation.binding;
        if binding.host != host || entry.permit.is_none() || entry.custody != Custody::Dispatched {
            return Err("native invocation has no live exact host custody".into());
        }
        Ok(binding)
    }

    /// Recheck signed retained receipts before accepting disk state.
    /// Verify retained admission proofs against the room and governor.
    ///
    /// # Errors
    /// Returns an error if retained signatures, bindings, custody or capacity are invalid.
    pub fn verify_retained(&self, room: RoomId, governor: [u8; 32]) -> Result<(), String> {
        self.validate_confirmations()?;
        self.validate_model_hosts(room)?;
        self.validate_model_deliveries(room)?;
        if self.reservations.len() > MAX_RESERVATIONS
            || self.fences.len() > MAX_RESERVATIONS
            || self.acknowledgements.len() > MAX_RESERVATIONS
        {
            return Err("custody capacity exceeded".into());
        }
        for entry in self.reservations.values() {
            entry.reservation.verify()?;
            if entry.reservation.binding.room != room || entry.reservation.governor != governor {
                return Err("retained invocation governor mismatch".into());
            }
            if let Some(completion) = &entry.completion {
                completion.verify()?;
                if entry.permit.as_ref() != Some(&completion.permit)
                    || completion.host != entry.reservation.binding.host
                {
                    return Err("retained terminal custody mismatch".into());
                }
            }
            if let Some(permit) = &entry.permit {
                permit.verify()?;
                if permit.reservation != entry.reservation {
                    return Err("retained permit mismatch".into());
                }
            }
        }
        for fence in self.fences.values() {
            fence.verify()?;
            if fence.room != room || fence.governor != governor {
                return Err("retained fence governor mismatch".into());
            }
        }
        for acks in self.acknowledgements.values() {
            for ack in acks.values() {
                ack.verify()?;
            }
        }
        Ok(())
    }
    /// Governor admission truth, independent of model acknowledgements.
    pub fn state(&self, task: &str, epoch: u64) -> NativeTaskAdmissionState {
        let run = run_key(task, epoch);
        let Some(fence) = self.fences.get(&run) else {
            return NativeTaskAdmissionState::Open;
        };
        let complete = fence.hosts.iter().all(|host| {
            self.acknowledgements
                .get(&run)
                .is_some_and(|acks| acks.contains_key(&format!("{host:?}")))
        });
        let unresolved = self.reservations.values().any(|entry| {
            entry.reservation.binding.task == task
                && entry.reservation.binding.epoch == epoch
                && entry.custody == Custody::Reserved
        });
        if complete && !unresolved {
            NativeTaskAdmissionState::Stopped
        } else {
            NativeTaskAdmissionState::Stopping
        }
    }
    /// Fences this physical host has not yet durably acknowledged.
    pub fn pending_host_fences(&self, host: [u8; 32]) -> Vec<NativeTaskFence> {
        self.fences
            .iter()
            .filter(|(run, fence)| {
                fence.hosts.contains(&host)
                    && !self
                        .acknowledgements
                        .get(*run)
                        .is_some_and(|acks| acks.contains_key(&format!("{host:?}")))
            })
            .map(|(_, fence)| fence.clone())
            .collect()
    }
    pub(crate) fn is_fenced(&self, task: &str, epoch: u64) -> bool {
        self.fences.contains_key(&run_key(task, epoch))
    }
}

pub(crate) fn validate(view: &NativeCommittedView) -> Result<(), String> {
    let ledger = &view.record.task_admission;
    ledger.validate_confirmations()?;
    ledger.validate_model_hosts(view.record.room)?;
    ledger.validate_model_deliveries(view.record.room)?;
    if view
        .record
        .private_task_mailboxes
        .messages
        .values()
        .any(|message| {
            message.status == tilo_actions::task_messaging::TaskMessageStatus::DeliveredToModel
                && !ledger.model_delivered_message(message)
        })
    {
        return Err("native MODEL delivery metadata lacks retained signed proof".into());
    }
    if ledger.reservations.len() > MAX_RESERVATIONS
        || ledger.fences.len() > MAX_RESERVATIONS
        || ledger.acknowledgements.len() > MAX_RESERVATIONS
        || ledger.confirmations.len() > MAX_RESERVATIONS
    {
        return Err("native admission ledger exceeds custody capacity".into());
    }
    if view.record.version < 5
        && (ledger.serial != 0
            || !ledger.reservations.is_empty()
            || !ledger.fences.is_empty()
            || !ledger.acknowledgements.is_empty()
            || !ledger.confirmations.is_empty()
            || !ledger.model_hosts.is_empty()
            || !ledger.model_deliveries.is_empty())
    {
        return Err("native admission custody requires a versioned owner record".into());
    }
    let mut requests: BTreeMap<uuid::Uuid, &NativeInvocationBinding> = BTreeMap::new();
    for (id, entry) in &ledger.reservations {
        let reservation = &entry.reservation;
        reservation.verify()?;
        if let Some(completion) = &entry.completion {
            completion.verify()?;
            if entry.permit.as_ref() != Some(&completion.permit)
                || completion.host != reservation.binding.host
            {
                return Err("native retained terminal custody mismatch".into());
            }
        }
        if let Some(permit) = &entry.permit {
            permit.verify()?;
            if permit.reservation != *reservation
                || permit.serial <= reservation.serial
                || permit.continuity < reservation.continuity
            {
                return Err("native retained dispatch permit binding is invalid".into());
            }
        } else if entry.host_started || entry.custody == Custody::Dispatched {
            return Err("native dispatch has no governor permit".into());
        }
        if *id != reservation.binding.invocation
            || id.is_nil()
            || reservation.binding.request.is_nil()
            || requests
                .get(&reservation.binding.request)
                .is_some_and(|previous| !confirmation::same_request(previous, &reservation.binding))
            || reservation.binding.room != view.record.room
            || reservation.governor != view.governor().device_public_key
            || reservation.serial == 0
            || reservation.continuity == 0
        {
            return Err("native retained reservation binding is invalid".into());
        }
        requests.insert(reservation.binding.request, &reservation.binding);
    }
    for (run, fence) in &ledger.fences {
        fence.verify()?;
        if *run != run_key(&fence.task, fence.epoch)
            || fence.room != view.record.room
            || fence.governor != view.governor().device_public_key
            || fence.serial == 0
            || fence.continuity == 0
        {
            return Err("native retained stop fence binding is invalid".into());
        }
    }
    for (run, acks) in &ledger.acknowledgements {
        for (host, ack) in acks {
            ack.verify()?;
            if ledger.fences.get(run) != Some(&ack.fence)
                || *host != format!("{:?}", ack.host)
                || !ack.fence.hosts.contains(&ack.host)
            {
                return Err("native retained host acknowledgement binding is invalid".into());
            }
        }
    }
    Ok(())
}

/// Governor-side custody transaction. The caller must hold its production room
/// writer lock and atomically persist the changed ledger before answering.
pub struct NativeGovernorAdmission<'a> {
    pub room: RoomId,
    pub peer: &'a crate::StatePeer,
    pub ledger: &'a mut TaskAdmissionLedger,
    pub continuity: u64,
    pub key: &'a SigningKey,
}
impl NativeGovernorAdmission<'_> {
    /// Retain the exact host's terminal digest without replaying any effect.
    /// # Errors
    /// Returns an error if host custody, signatures or the retained dispatch do not match.
    pub fn complete(
        &mut self,
        completion: &NativeInvocationCompletion,
        source: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.custody()?;
        self.host(source, now)?;
        completion.verify()?;
        let binding = &completion.permit.reservation.binding;
        let source_device = source.device_public_key;
        if completion.host != source_device
            || binding.host != completion.host
            || binding.room != self.room
        {
            return Err("completion physical host mismatch".into());
        }
        let entry = self
            .ledger
            .reservations
            .get_mut(&binding.invocation)
            .ok_or("completion reservation absent")?;
        if entry.permit.as_ref() != Some(&completion.permit) || entry.custody != Custody::Dispatched
        {
            return Err("completion has no exact dispatch custody".into());
        }
        if let Some(prior) = &entry.completion {
            if prior != completion {
                return Err("terminal digest changed".into());
            }
        } else {
            entry.completion = Some(completion.clone());
        }
        Ok(())
    }
    fn custody(&self) -> Result<(), String> {
        if self.key.verifying_key().to_bytes() != self.peer.authority().device_public_key
            || self.peer.room() != self.room.to_string()
            || self.continuity == 0
        {
            return Err("native admission requires current governor custody".into());
        }
        self.peer
            .permissions(self.peer.authority().device_public_key, SystemTime::now())?;
        Ok(())
    }
    fn host(&self, host: &DeviceCert, now: SystemTime) -> Result<(), String> {
        tilo_user_identity::verify_device_cert_at(host, tilo_clock::unix_now_ns())
            .map_err(|error| error.to_string())?;
        if (!host.capabilities.can_act_as_host && host != self.peer.authority())
            || !self.peer.certificates().contains(host)
            || host.user_public_key != self.peer.authority().user_public_key
        {
            return Err("native admission host is not current owner-certified evidence".into());
        }
        self.peer.permissions(host.device_public_key, now)?;
        Ok(())
    }
    /// # Errors
    /// Returns an error if custody, binding, current policy or the task frontier is invalid.
    pub fn reserve(
        &mut self,
        binding: NativeInvocationBinding,
        host: &DeviceCert,
        now: SystemTime,
        check: impl FnOnce(&crate::StatePeer, &NativeInvocationBinding) -> Result<(), String>,
    ) -> Result<NativeInvocationReservation, String> {
        self.custody()?;
        self.host(host, now)?;
        let host_device = host.device_public_key;
        if binding.room != self.room
            || binding.invocation.is_nil()
            || binding.request.is_nil()
            || binding.host != host_device
            || binding.app.is_empty()
            || binding.provider.is_empty()
            || binding.private_run_digest == [0; 32]
            || binding.contract_digest == [0; 32]
            || binding.ceiling_digest == [0; 32]
        {
            return Err("native invocation binding is incomplete".into());
        }
        check(self.peer, &binding)?;
        let ledger = &*self.ledger;
        if ledger
            .fences
            .contains_key(&run_key(&binding.task, binding.epoch))
        {
            return Err("native task admission is stopped".into());
        }
        if ledger.reservations.iter().any(|(id, entry)| {
            *id != binding.invocation
                && entry.reservation.binding.request == binding.request
                && !confirmation::same_request(&entry.reservation.binding, &binding)
        }) || (ledger.reservations.len() >= MAX_RESERVATIONS
            && !ledger.reservations.contains_key(&binding.invocation))
        {
            return Err("native request already bound or reservation capacity exhausted".into());
        }
        let doc = self
            .peer
            .admitted_room()
            .ok_or("native task document absent")?;
        let snapshot = tilo_peer_state::workspace_task::snapshot(doc)?;
        let control = snapshot
            .controls
            .get(&binding.task)
            .ok_or("native task control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("native task run absent")?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == binding.task)
            .ok_or("native task absent")?;
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
            || task.assigned_agent.as_deref() != Some(binding.agent.as_str())
            || run.agent_id != binding.agent
            || run.epoch != binding.epoch
            || !binding.matches_frontier(
                control
                    .directions
                    .last()
                    .map_or(0, |direction| direction.revision),
                run.applied_direction_revision,
            )?
        {
            return Err("native reservation task frontier is stale".into());
        }
        self.peer.workspace().check_task_execution_actor(
            &binding.agent,
            run.approval.as_ref(),
            now,
        )?;
        if let Some(entry) = ledger.reservations.get(&binding.invocation) {
            if entry.reservation.binding != binding || entry.custody != Custody::Reserved {
                return Err(
                    "native invocation is already bound or dispatched; do not retry effects".into(),
                );
            }
            return Ok(entry.reservation.clone());
        }
        self.ledger.consume_confirmation(&binding)?;
        let mut reservation = NativeInvocationReservation {
            binding,
            governor: self.peer.authority().device_public_key,
            continuity: self.continuity,
            serial: next_serial(self.ledger)?,
            signature: [[0; 32]; 2],
        };
        reservation.signature = sign(
            "tilo-native-invocation-reservation-v1",
            &reservation.payload(),
            self.key,
        )?;
        self.ledger.reservations.insert(
            reservation.binding.invocation,
            Entry {
                reservation: reservation.clone(),
                permit: None,
                host_started: false,
                completion: None,
                custody: Custody::Reserved,
            },
        );
        Ok(reservation)
    }

    /// # Errors
    /// Returns an error if the reservation, host, policy or current task frontier is invalid.
    pub fn claim(
        &mut self,
        reservation: &NativeInvocationReservation,
        host: &DeviceCert,
        now: SystemTime,
        check: impl FnOnce(&crate::StatePeer, &NativeInvocationBinding) -> Result<(), String>,
    ) -> Result<NativeInvocationPermit, String> {
        reservation.verify()?;
        let binding = &reservation.binding;
        self.custody()?;
        self.host(host, now)?;
        if host.device_public_key != binding.host
            || reservation.governor != self.peer.authority().device_public_key
        {
            return Err("native dispatch host or governor mismatch".into());
        }
        check(self.peer, binding)?;
        let document = self
            .peer
            .admitted_room()
            .ok_or("native task document absent")?;
        let snapshot = tilo_peer_state::workspace_task::snapshot(document)?;
        let control = snapshot
            .controls
            .get(&binding.task)
            .ok_or("native task control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("native task run absent")?;
        let assigned_agent = run.agent_id.as_str();
        if assigned_agent != binding.agent
            || run.epoch != binding.epoch
            || !binding.matches_frontier(
                control
                    .directions
                    .last()
                    .map_or(0, |direction| direction.revision),
                run.applied_direction_revision,
            )?
        {
            return Err("native dispatch frontier is stale".into());
        }
        self.peer.workspace().check_task_execution_actor(
            &binding.agent,
            run.approval.as_ref(),
            now,
        )?;
        let ledger = &*self.ledger;
        if ledger.is_fenced(&binding.task, binding.epoch) {
            return Err("native dispatch admission is stopped".into());
        }
        let entry = ledger
            .reservations
            .get(&binding.invocation)
            .ok_or("native dispatch reservation absent")?;
        if entry.reservation != *reservation || entry.custody == Custody::Rejected {
            return Err("native dispatch reservation binding changed".into());
        }
        if let Some(permit) = &entry.permit {
            return Ok(permit.clone());
        }
        let mut permit = NativeInvocationPermit {
            reservation: reservation.clone(),
            continuity: self.continuity,
            serial: next_serial(self.ledger)?,
            signature: [[0; 32]; 2],
        };
        permit.signature = sign(
            "tilo-native-invocation-permit-v1",
            &permit.payload(),
            self.key,
        )?;
        let entry = self
            .ledger
            .reservations
            .get_mut(&binding.invocation)
            .ok_or("native dispatch reservation lost")?;
        entry.permit = Some(permit.clone());
        entry.custody = Custody::Dispatched;
        Ok(permit)
    }

    /// # Errors
    /// Returns an error if custody or the task frontier is invalid, or signing fails.
    pub fn stop(&mut self, task: &str, epoch: u64) -> Result<NativeTaskFence, String> {
        self.stop_with_local_host(task, epoch, None)
    }

    pub(crate) fn stop_with_local_host(
        &mut self,
        task: &str,
        epoch: u64,
        local_host: Option<[u8; 32]>,
    ) -> Result<NativeTaskFence, String> {
        let room = self.room;
        self.custody()?;
        if local_host.is_some_and(|host| host != self.peer.authority().device_public_key) {
            return Err("local Stop host differs from governor custody".into());
        }
        let run = run_key(task, epoch);
        let document = self
            .peer
            .admitted_room()
            .ok_or("native task document absent")?;
        let snapshot = tilo_peer_state::workspace_task::snapshot(document)?;
        if snapshot
            .controls
            .get(task)
            .is_none_or(|control| control.run_epoch != epoch)
        {
            return Err("native stop task frontier is stale".into());
        }
        if let Some(fence) = self.ledger.fences.get(&run) {
            return Ok(fence.clone());
        }
        let hosts = self
            .ledger
            .reservations
            .values()
            .filter(|entry| {
                entry.reservation.binding.task == task && entry.reservation.binding.epoch == epoch
            })
            .map(|entry| entry.reservation.binding.host)
            .chain(
                self.ledger
                    .model_hosts
                    .values()
                    .filter(|registration| {
                        registration.context.task == task && registration.context.epoch == epoch
                    })
                    .map(|registration| registration.host),
            )
            // Local preparation exposes no reservation. The physical governor
            // must still durably acknowledge its queued/unstarted work barrier.
            .chain(local_host)
            .collect();
        let mut fence = NativeTaskFence {
            room,
            task: task.into(),
            epoch,
            governor: self.peer.authority().device_public_key,
            continuity: self.continuity,
            serial: next_serial(self.ledger)?,
            hosts,
            signature: [[0; 32]; 2],
        };
        fence.signature = sign("tilo-native-task-fence-v1", &fence.payload(), self.key)?;
        self.ledger.fences.insert(run, fence.clone());
        Ok(fence)
    }

    /// # Errors
    /// Returns an error if the host acknowledgement does not match retained fence custody.
    pub fn receive_fence(
        &mut self,
        ack: &NativeHostFenceAcknowledgement,
        source: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.custody()?;
        ack.verify()?;
        if source.device_public_key != ack.host {
            return Err("native fence acknowledgement source mismatch".into());
        }
        self.host(source, now)?;
        let run = run_key(&ack.fence.task, ack.fence.epoch);
        let ledger = &mut *self.ledger;
        if ledger.fences.get(&run) != Some(&ack.fence) || !ack.fence.hosts.contains(&ack.host) {
            return Err("native fence acknowledgement has no exact retained stop".into());
        }
        for id in &ack.dispatched {
            let entry = ledger
                .reservations
                .get(id)
                .ok_or("native host reports unknown dispatch")?;
            let binding = &entry.reservation.binding;
            if entry.permit.is_none()
                || binding.host != ack.host
                || binding.task != ack.fence.task
                || binding.epoch != ack.fence.epoch
            {
                return Err("native host reports foreign dispatch custody".into());
            }
        }
        let acks = ledger.acknowledgements.entry(run).or_default();
        let host_id = format!("{:?}", ack.host);
        if let Some(existing) = acks.get(&host_id) {
            return if existing == ack {
                Ok(())
            } else {
                Err("native host fence acknowledgement changed".into())
            };
        }
        acks.insert(host_id, ack.clone());
        for (id, entry) in &mut ledger.reservations {
            let binding = &entry.reservation.binding;
            if binding.host == ack.host
                && binding.task == ack.fence.task
                && binding.epoch == ack.fence.epoch
            {
                entry.custody = if ack.dispatched.contains(id) {
                    Custody::Dispatched
                } else {
                    Custody::Rejected
                };
            }
        }
        Ok(())
    }
}
fn run_key(task: &str, epoch: u64) -> String {
    format!("{}:{task}:{epoch}", task.len())
}
fn signing_bytes<T: Serialize>(domain: &str, payload: &T) -> Result<Vec<u8>, String> {
    postcard::to_stdvec(&(domain, payload)).map_err(|error| error.to_string())
}
fn sign<T: Serialize>(
    domain: &str,
    payload: &T,
    key: &SigningKey,
) -> Result<[[u8; 32]; 2], String> {
    let bytes = key.sign(&signing_bytes(domain, payload)?).to_bytes();
    let mut signature = [[0; 32]; 2];
    signature[0].copy_from_slice(&bytes[..32]);
    signature[1].copy_from_slice(&bytes[32..]);
    Ok(signature)
}
fn verify<T: Serialize>(
    domain: &str,
    payload: &T,
    public: [u8; 32],
    signature: [[u8; 32]; 2],
) -> Result<(), String> {
    let mut bytes = [0; 64];
    bytes[..32].copy_from_slice(&signature[0]);
    bytes[32..].copy_from_slice(&signature[1]);
    VerifyingKey::from_bytes(&public)
        .map_err(|error| error.to_string())?
        .verify_strict(
            &signing_bytes(domain, payload)?,
            &Signature::from_bytes(&bytes),
        )
        .map_err(|error| error.to_string())
}
impl NativeInvocationReservation {
    fn payload(&self) -> (&NativeInvocationBinding, [u8; 32], u64, u64) {
        (&self.binding, self.governor, self.continuity, self.serial)
    }
    /// Verify the reservation's transport signature.
    /// Callers must separately pin `governor` to their authenticated authority.
    ///
    /// # Errors
    /// Returns an error for invalid signing keys, payload encoding or signatures.
    pub fn verify(&self) -> Result<(), String> {
        verify(
            "tilo-native-invocation-reservation-v1",
            &self.payload(),
            self.governor,
            self.signature,
        )
    }
}
impl NativeInvocationPermit {
    fn payload(&self) -> (&NativeInvocationReservation, u64, u64) {
        (&self.reservation, self.continuity, self.serial)
    }
    /// Verify both the reservation and permit transport signatures.
    /// Callers must separately pin the reservation's governor to their authority.
    ///
    /// # Errors
    /// Returns an error if either payload, signing key or signature is invalid.
    pub fn verify(&self) -> Result<(), String> {
        self.reservation.verify()?;
        verify(
            "tilo-native-invocation-permit-v1",
            &self.payload(),
            self.reservation.governor,
            self.signature,
        )
    }
}
type NativeTaskFencePayload<'a> = (
    RoomId,
    &'a str,
    u64,
    [u8; 32],
    u64,
    u64,
    &'a BTreeSet<[u8; 32]>,
);

impl NativeTaskFence {
    fn payload(&self) -> NativeTaskFencePayload<'_> {
        (
            self.room,
            &self.task,
            self.epoch,
            self.governor,
            self.continuity,
            self.serial,
            &self.hosts,
        )
    }
    fn verify(&self) -> Result<(), String> {
        verify(
            "tilo-native-task-fence-v1",
            &self.payload(),
            self.governor,
            self.signature,
        )
    }
}
impl NativeHostFenceAcknowledgement {
    fn payload(&self) -> (&NativeTaskFence, [u8; 32], &BTreeSet<uuid::Uuid>) {
        (&self.fence, self.host, &self.dispatched)
    }
    fn verify(&self) -> Result<(), String> {
        self.fence.verify()?;
        verify(
            "tilo-native-host-fence-v1",
            &self.payload(),
            self.host,
            self.signature,
        )
    }
}

fn governor_custody(view: &NativeCommittedView, key: &SigningKey) -> Result<(), String> {
    if view.device() != view.governor()
        || key.verifying_key().to_bytes() != view.governor().device_public_key
    {
        return Err("native admission requires exact governor signing custody".into());
    }
    Ok(())
}
fn host_evidence(
    view: &NativeCommittedView,
    host: &DeviceCert,
    now: SystemTime,
) -> Result<(), String> {
    if (!host.capabilities.can_act_as_host && host != view.governor())
        || !view.peer().certificates().contains(host)
        || host.user_public_key != view.governor().user_public_key
    {
        return Err("native admission host is not current owner-certified evidence".into());
    }
    view.peer().permissions(host.device_public_key, now)?;
    Ok(())
}
fn next_serial(ledger: &mut TaskAdmissionLedger) -> Result<u64, String> {
    ledger.serial = ledger
        .serial
        .checked_add(1)
        .ok_or("native admission serial exhausted")?;
    Ok(ledger.serial)
}

/// Validate and consume host custody in a candidate; the caller must publish it
/// before executing the effect, while retaining the same owner mutation turn.
fn consume_local_candidate(
    candidate: &mut NativeCommittedView,
    permit: &NativeInvocationPermit,
    exact: &NativeInvocationBinding,
    source: &DeviceCert,
    check: impl FnOnce(&NativeCommittedView, SystemTime) -> Result<(), String>,
) -> Result<(), String> {
    permit.verify()?;
    let reservation = &permit.reservation;
    if exact != &reservation.binding
        || source != candidate.governor()
        || reservation.governor != source.device_public_key
        || reservation.binding.host != candidate.device().device_public_key
        || (!candidate.device().capabilities.can_act_as_host
            && candidate.device() != candidate.governor())
    {
        return Err("native reservation source or local host binding mismatch".into());
    }
    let now = SystemTime::now();
    host_evidence(candidate, candidate.device(), now)?;
    let snapshot = tilo_peer_state::workspace_task::snapshot(
        candidate
            .peer()
            .admitted_room()
            .ok_or("native host task document absent")?,
    )?;
    let control = snapshot
        .controls
        .get(&exact.task)
        .ok_or("native host control absent")?;
    let run = control
        .active_run
        .as_ref()
        .ok_or("native host run absent")?;
    let assigned_agent = run.agent_id.as_str();
    if run.epoch != exact.epoch
        || assigned_agent != exact.agent
        || !exact.matches_frontier(
            control
                .directions
                .last()
                .map_or(0, |direction| direction.revision),
            run.applied_direction_revision,
        )?
    {
        return Err("native host run frontier changed".into());
    }
    candidate.peer().workspace().check_task_execution_actor(
        &exact.agent,
        run.approval.as_ref(),
        now,
    )?;
    check(candidate, now)?;
    let ledger = &mut candidate.record.task_admission;
    if ledger
        .fences
        .contains_key(&run_key(&exact.task, exact.epoch))
        || ledger
            .reservations
            .get(&exact.invocation)
            .is_some_and(|entry| {
                entry.reservation != *reservation
                    || entry.permit.as_ref() != Some(permit)
                    || entry.host_started
                    || entry.custody == Custody::Rejected
            })
        || ledger.reservations.iter().any(|(id, entry)| {
            *id != exact.invocation
                && entry.reservation.binding.request == exact.request
                && !confirmation::same_request(&entry.reservation.binding, exact)
        })
        || (ledger.reservations.len() >= MAX_RESERVATIONS
            && !ledger.reservations.contains_key(&exact.invocation))
    {
        return Err("native host fenced, duplicate request or dispatch custody unknown".into());
    }
    ledger.reservations.insert(
        exact.invocation,
        Entry {
            reservation: reservation.clone(),
            permit: Some(permit.clone()),
            host_started: true,
            completion: None,
            custody: Custody::Dispatched,
        },
    );

    Ok(())
}

impl NativeWorkspaceOwner {
    /// Sign and durably retain a terminal digest for one consumed local permit.
    /// # Errors
    /// Returns an error if dispatch custody is absent, the digest changed or persistence fails.
    pub fn complete_native_invocation(
        &self,
        room: RoomId,
        invocation: uuid::Uuid,
        result_digest: [u8; 32],
        key: &SigningKey,
    ) -> Result<NativeInvocationCompletion, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let host = key.verifying_key().to_bytes();
        let entry = candidate
            .record
            .task_admission
            .reservations
            .get_mut(&invocation)
            .ok_or("terminal reservation absent")?;
        if !entry.host_started
            || entry.reservation.binding.host != host
            || entry.custody != Custody::Dispatched
        {
            return Err("terminal local custody absent".into());
        }
        if let Some(completion) = &entry.completion {
            if completion.result_digest != result_digest {
                return Err("terminal digest changed".into());
            }
            return Ok(completion.clone());
        }
        let mut completion = NativeInvocationCompletion {
            permit: entry.permit.clone().ok_or("terminal permit absent")?,
            result_digest,
            host,
            signature: [[0; 32]; 2],
        };
        completion.signature = sign(
            "tilo-native-completion-v1",
            &(&completion.permit, completion.result_digest, host),
            key,
        )?;
        entry.completion = Some(completion.clone());
        self.publish_task_admission(room, candidate)?;
        Ok(completion)
    }
    fn publish_task_admission(
        &self,
        room: RoomId,
        mut candidate: NativeCommittedView,
    ) -> Result<(), String> {
        // Old readers must refuse custody they cannot enforce, not ignore it.
        candidate.record.version = 5;
        candidate.record.peer = candidate.peer.durable()?;
        validate(&candidate)?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native admission generation exhausted")?;
        self.publish_room(room, candidate, None)
    }

    /// Persist one exact reservation before returning its signed receipt. The
    /// supplied policy check runs under the same owner turn, on current state.
    /// It must validate the whole action ceiling, not combine two actors' grants.
    ///
    /// # Errors
    /// Stale scope/run, Stop, changed retries, missing custody or publication failure.
    pub fn reserve_native_invocation(
        &self,
        binding: NativeInvocationBinding,
        host: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
        check: impl FnOnce(&NativeCommittedView, &NativeInvocationBinding) -> Result<(), String>,
    ) -> Result<NativeInvocationReservation, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(binding.room, |view| Ok(view.clone()))?;
        governor_custody(&candidate, key)?;
        crate::enrollment::ensure_private_task_baseline(&mut candidate, key, now)?;
        let policy_view = candidate.clone();
        let continuity = candidate.continuity();
        let mut admission = NativeGovernorAdmission {
            room: binding.room,
            peer: &candidate.peer,
            ledger: &mut candidate.record.task_admission,
            continuity,
            key,
        };
        let result = admission.reserve(binding, host, now, |_, binding| {
            check(&policy_view, binding)
        })?;
        self.publish_task_admission(result.binding.room, candidate)?;
        Ok(result)
    }

    /// Recheck the full current action ceiling and record dispatch custody in
    /// the governor's serialized transaction. An unclaimed reservation is inert.
    /// # Errors
    /// Stop, revoked/expired actors or scope, changed binding or storage failure.
    pub fn claim_native_invocation(
        &self,
        reservation: &NativeInvocationReservation,
        host: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
        check: impl FnOnce(&NativeCommittedView, &NativeInvocationBinding) -> Result<(), String>,
    ) -> Result<NativeInvocationPermit, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(reservation.binding.room, |view| Ok(view.clone()))?;
        governor_custody(&candidate, key)?;
        crate::enrollment::ensure_private_task_baseline(&mut candidate, key, now)?;
        let policy_view = candidate.clone();
        let continuity = candidate.continuity();
        let mut admission = NativeGovernorAdmission {
            room: reservation.binding.room,
            peer: &candidate.peer,
            ledger: &mut candidate.record.task_admission,
            continuity,
            key,
        };
        let result = admission.claim(reservation, host, now, |_, binding| {
            check(&policy_view, binding)
        })?;
        self.publish_task_admission(reservation.binding.room, candidate)?;
        Ok(result)
    }

    /// Reserve, claim and consume one exact local invocation in one durable turn.
    /// Intermediate permits never escape before publication. The effect runs only
    /// after the complete consumed record is durable, still serialized with Stop.
    /// A crash or callback error leaves unknown/consumed custody, never replayable.
    /// # Errors
    /// Foreign host, changed authority, replay, Stop or failed durable publication.
    pub fn admit_local_native_invocation<T>(
        &self,
        binding: NativeInvocationBinding,
        host: &DeviceCert,
        key: &SigningKey,
        check: impl Fn(&NativeCommittedView, &NativeInvocationBinding, SystemTime) -> Result<(), String>,
        admit: impl FnOnce() -> Result<T, String>,
    ) -> Result<(T, NativeInvocationPermit), String> {
        let _turn = self.0.mutation.lock();
        let room = binding.room;
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        governor_custody(&candidate, key)?;
        if host != candidate.device() || binding.host != candidate.device().device_public_key {
            return Err("native atomic admission requires the governor's own physical host".into());
        }
        let now = SystemTime::now();
        crate::enrollment::ensure_private_task_baseline(&mut candidate, key, now)?;
        let policy_view = candidate.clone();
        let continuity = candidate.continuity();
        let mut admission = NativeGovernorAdmission {
            room,
            peer: &candidate.peer,
            ledger: &mut candidate.record.task_admission,
            continuity,
            key,
        };
        let reservation = admission.reserve(binding, host, now, |_, exact| {
            check(&policy_view, exact, now)
        })?;
        let permit = admission.claim(&reservation, host, now, |_, exact| {
            check(&policy_view, exact, now)
        })?;
        consume_local_candidate(
            &mut candidate,
            &permit,
            &reservation.binding,
            host,
            |view, now| check(view, &reservation.binding, now),
        )?;
        self.publish_task_admission(room, candidate)?;
        let result = admit()?;
        Ok((result, permit))
    }

    /// Accept Stop immediately at the governor. Only signed host fence receipts
    /// can complete it; no clock timeout or model response can turn unknown into Stopped.
    /// # Errors
    /// Missing exact governor custody, wrong run or failed publication.
    pub fn stop_native_admission(
        &self,
        room: RoomId,
        task: &str,
        epoch: u64,
        key: &SigningKey,
    ) -> Result<NativeTaskFence, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        governor_custody(&candidate, key)?;
        let host = candidate.device().device_public_key;
        let continuity = candidate.continuity();
        let mut admission = NativeGovernorAdmission {
            room,
            peer: &candidate.peer,
            ledger: &mut candidate.record.task_admission,
            continuity,
            key,
        };
        let result = admission.stop_with_local_host(task, epoch, Some(host))?;
        self.publish_task_admission(room, candidate)?;
        Ok(result)
    }

    /// Run only on the certificate-bound local host with a consumed governor
    /// permit, not an inert reservation. Dispatch intent is durable
    /// before the callback, and survives callback error or process death as in
    /// flight/unknown. Reconnection cannot replay the effect.
    /// # Errors
    /// Source/metadata mismatch, duplicate dispatch, a host fence or storage failure.
    #[cfg(test)]
    pub fn admit_reserved_native_invocation<T>(
        &self,
        permit: &NativeInvocationPermit,
        exact: &NativeInvocationBinding,
        source: &DeviceCert,
        admit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.admit_reserved_native_invocation_checked(permit, exact, source, |_, _| Ok(()), admit)
    }

    /// Recheck effect-specific live authority inside the host dispatch/fence turn.
    /// # Errors
    /// Rejects changed policy, revoked scope, replay, or a fenced invocation.
    pub fn admit_reserved_native_invocation_checked<T>(
        &self,
        permit: &NativeInvocationPermit,
        exact: &NativeInvocationBinding,
        source: &DeviceCert,
        check: impl FnOnce(&crate::NativeCommittedView, SystemTime) -> Result<(), String>,
        admit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        permit.verify()?;
        let reservation = &permit.reservation;
        let _turn = self.0.mutation.lock();
        let room = reservation.binding.room;
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        consume_local_candidate(&mut candidate, permit, exact, source, check)?;
        self.publish_task_admission(room, candidate)?;
        admit()
    }

    /// Persist the local host fence, reject queued/unstarted work, then sign its
    /// acknowledgement. Serialized with dispatch; never waits for a model.
    /// # Errors
    /// Wrong authenticated governor/local host or failed durable publication.
    pub fn acknowledge_native_host_fence(
        &self,
        fence: &NativeTaskFence,
        source: &DeviceCert,
        key: &SigningKey,
    ) -> Result<NativeHostFenceAcknowledgement, String> {
        fence.verify()?;
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(fence.room, |view| Ok(view.clone()))?;
        let host = key.verifying_key().to_bytes();
        if source != candidate.governor()
            || fence.governor != source.device_public_key
            || host != candidate.device().device_public_key
            || !fence.hosts.contains(&host)
        {
            return Err("native fence source or host mismatch".into());
        }
        let run = run_key(&fence.task, fence.epoch);
        let ledger = &mut candidate.record.task_admission;
        if let Some(existing) = ledger
            .acknowledgements
            .get(&run)
            .and_then(|acks| acks.get(&format!("{host:?}")))
        {
            return if existing.fence == *fence {
                Ok(existing.clone())
            } else {
                Err("native fence changed for retained host acknowledgement".into())
            };
        }
        if let Some(existing) = ledger.fences.get(&run)
            && existing != fence
        {
            return Err("native fence changed for retained run".into());
        }
        let mut dispatched = BTreeSet::new();
        for (id, entry) in &mut ledger.reservations {
            if entry.reservation.binding.task == fence.task
                && entry.reservation.binding.epoch == fence.epoch
            {
                if entry.reservation.continuity > fence.continuity
                    || entry.reservation.serial >= fence.serial
                    || entry.permit.as_ref().is_some_and(|permit| {
                        permit.continuity > fence.continuity || permit.serial >= fence.serial
                    })
                {
                    return Err("native host has unresolved custody outside stop frontier".into());
                }
                if entry.host_started {
                    dispatched.insert(*id);
                } else {
                    entry.custody = Custody::Rejected;
                }
            }
        }
        ledger.fences.insert(run, fence.clone());
        let mut ack = NativeHostFenceAcknowledgement {
            fence: fence.clone(),
            host,
            dispatched,
            signature: [[0; 32]; 2],
        };
        ack.signature = sign("tilo-native-host-fence-v1", &ack.payload(), key)?;
        candidate
            .record
            .task_admission
            .acknowledgements
            .entry(run_key(&fence.task, fence.epoch))
            .or_default()
            .insert(format!("{host:?}"), ack.clone());
        self.publish_task_admission(fence.room, candidate)?;
        Ok(ack)
    }

    /// Consume the exact authenticated host acknowledgement at the governor.
    /// # Errors
    /// Forged/source-mismatched ack, unknown custody, changed frontier or failed storage.
    pub fn receive_native_host_fence(
        &self,
        ack: &NativeHostFenceAcknowledgement,
        source: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        ack.verify()?;
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(ack.fence.room, |view| Ok(view.clone()))?;
        let source_device = source.device_public_key;
        if candidate.device() != candidate.governor() || source_device != ack.host {
            return Err("native fence acknowledgement source mismatch".into());
        }
        host_evidence(&candidate, source, now)?;
        let run = run_key(&ack.fence.task, ack.fence.epoch);
        let ledger = &mut candidate.record.task_admission;
        if ledger.fences.get(&run) != Some(&ack.fence) || !ack.fence.hosts.contains(&ack.host) {
            return Err("native fence acknowledgement has no exact retained stop".into());
        }
        for id in &ack.dispatched {
            let entry = ledger
                .reservations
                .get(id)
                .ok_or("native host reports unknown dispatch")?;
            let binding = &entry.reservation.binding;
            if entry.permit.is_none()
                || binding.host != ack.host
                || binding.task != ack.fence.task
                || binding.epoch != ack.fence.epoch
            {
                return Err("native host reports foreign dispatch custody".into());
            }
        }
        let acks = ledger.acknowledgements.entry(run).or_default();
        let host_id = format!("{:?}", ack.host);
        if let Some(existing) = acks.get(&host_id) {
            return if existing == ack {
                Ok(())
            } else {
                Err("native host fence acknowledgement changed".into())
            };
        }
        acks.insert(host_id, ack.clone());
        for (id, entry) in &mut ledger.reservations {
            let binding = &entry.reservation.binding;
            if binding.host == ack.host
                && binding.task == ack.fence.task
                && binding.epoch == ack.fence.epoch
            {
                entry.custody = if ack.dispatched.contains(id) {
                    Custody::Dispatched
                } else {
                    Custody::Rejected
                };
            }
        }
        self.publish_task_admission(ack.fence.room, candidate)
    }

    /// Admission-barrier truth only: Stopped makes no promise of effect rollback.
    /// # Errors
    /// Owner state unavailable/ambiguous. The caller must display unknown, not Stopped.
    pub fn native_task_admission_state(
        &self,
        room: RoomId,
        task: &str,
        epoch: u64,
    ) -> Result<NativeTaskAdmissionState, String> {
        self.with_current(room, |view| {
            if view.device() != view.governor() {
                return Err(
                    "native admission state requires the governing owner's custody ledger".into(),
                );
            }
            let ledger = &view.record.task_admission;
            let run = run_key(task, epoch);
            let Some(fence) = ledger.fences.get(&run) else {
                return Ok(NativeTaskAdmissionState::Open);
            };
            let complete = fence.hosts.iter().all(|host| {
                ledger
                    .acknowledgements
                    .get(&run)
                    .is_some_and(|acks| acks.contains_key(&format!("{host:?}")))
            });
            let unresolved = ledger.reservations.values().any(|entry| {
                entry.reservation.binding.task == task
                    && entry.reservation.binding.epoch == epoch
                    && entry.custody == Custody::Reserved
            });
            Ok(if complete && !unresolved {
                NativeTaskAdmissionState::Stopped
            } else {
                NativeTaskAdmissionState::Stopping
            })
        })
    }
}

impl std::fmt::Debug for NativeAdmissionCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeAdmissionCommand { private: redacted }")
    }
}

#[cfg(test)]
mod frontier_tests {
    use super::*;

    fn binding() -> NativeInvocationBinding {
        NativeInvocationBinding {
            room: RoomId::parse(crate::testing::ROOM).unwrap(),
            task: "task".into(),
            agent: "scribe".into(),
            epoch: 1,
            revision: 2,
            invocation: uuid::Uuid::from_u128(1),
            request: uuid::Uuid::from_u128(2),
            host: [1; 32],
            app: "so.tilo.agent".into(),
            provider: "local".into(),
            resource: String::new(),
            action: String::new(),
            arguments_digest: [2; 32],
            contract_digest: [3; 32],
            ceiling_digest: [4; 32],
            private_run_digest: [5; 32],
        }
    }

    #[test]
    fn model_prepares_desired_but_actions_require_exact_applied_revision() {
        let mut exact = binding();
        assert_eq!(exact.frontier().unwrap(), NativeInvocationFrontier::Model);
        assert!(exact.matches_frontier(2, 1).unwrap());
        assert!(!exact.matches_frontier(3, 2).unwrap());
        exact.resource = "document".into();
        exact.action = "insert".into();
        assert_eq!(exact.frontier().unwrap(), NativeInvocationFrontier::Action);
        assert!(!exact.matches_frontier(2, 1).unwrap());
        assert!(exact.matches_frontier(2, 2).unwrap());
        assert!(!exact.matches_frontier(3, 2).unwrap());
        assert!(!exact.matches_frontier(2, 3).unwrap());
        // A resource-less application action is still an action, not MODEL.
        exact.resource.clear();
        assert!(!exact.matches_frontier(2, 1).unwrap());
        exact.resource = "document".into();
        exact.action.clear();
        assert!(exact.matches_frontier(2, 1).is_err());
    }

    #[test]
    fn all_shared_custody_gates_recheck_the_typed_frontier() {
        let source = include_str!("task_admission.rs");
        let reserve = source.split("    pub fn reserve(\n").nth(1).unwrap();
        let claim = reserve.split("    pub fn claim(\n").nth(1).unwrap();
        let reserve = reserve.split("    pub fn claim(\n").next().unwrap();
        let claim = claim
            .split("        let ledger = &*self.ledger;")
            .next()
            .unwrap();
        for gate in [reserve, claim] {
            assert!(gate.contains("check(self.peer,"));
            assert!(gate.contains("binding.matches_frontier("));
            assert!(gate.contains("run.applied_direction_revision,"));
        }
        let dispatch = source
            .split("    pub fn admit_reserved_native_invocation_checked<T>(\n")
            .nth(1)
            .unwrap();
        assert!(
            dispatch.contains(
                "consume_local_candidate(&mut candidate, permit, exact, source, check)?;"
            )
        );
        let host = source
            .split("fn consume_local_candidate(\n")
            .nth(1)
            .unwrap()
            .split("    let ledger = &mut candidate.record.task_admission;")
            .next()
            .unwrap();
        assert!(host.contains("check(candidate, now)?;"));
        assert!(host.contains("exact.matches_frontier("));
        assert!(host.contains("run.applied_direction_revision,"));
    }
}
