//! Typed MODEL custody, independent of the application-action registry.
//!
//! The gateway resolves Settings/account/vault state before freezing a request.
//! Its protected connection must implement `NativeModelAuthority`; an app-supplied
//! actor, window, model selection or planning flag is not that authority.
use super::{NativeInvocationBinding, NativeInvocationPermit, NativeInvocationReservation};
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use tilo_action_policy::{ActionKind, Answer, Authority, Context, TreePath, roles};
use tilo_user_identity::DeviceCert;

/// Provider-independent selection. `contract` freezes all effect-bearing gateway
/// configuration, including endpoint, protocol, account identity and model options.
/// It contains no credential bytes. Local unauthenticated endpoints remain valid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenNativeModelSelection {
    pub app: String,
    pub provider: String,
    pub model: String,
    pub contract: serde_json::Value,
}

/// Host-owned source of model use. Task sources are real broker-published
/// resources and do not require an application or an originating window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum NativeModelSource {
    Window { window: TreePath },
    Task { task_id: String, resource: TreePath },
}

/// The actual host-published requestModel contract, not an `AppAction`.
/// `window` retains its wire/constructor name for compatibility; for a Task
/// source it contains the task resource node, never a fabricated window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelContract {
    pub window: TreePath,
    pub request_model: TreePath,
    pub kind: ActionKind,
    pub selection: FrozenNativeModelSelection,
}

impl NativeModelContract {
    /// Read the exact shell snapshot approved at person start at the latest
    /// desired revision, before the resulting plan is acknowledged as applied.
    /// This is not a dispatch permit; policy and custody are rechecked at enqueue.
    /// # Errors
    /// Returns an error if current authority, run binding or durable custody cannot be verified.
    pub fn approved_for_run(
        view: &NativeCommittedView,
        binding: &crate::task_policy::NativeTaskBinding,
    ) -> Result<Self, String> {
        let mut state = tilo_peer_state::workspace_task::snapshot(
            view.peer()
                .admitted_room()
                .ok_or("native MODEL room absent")?,
        )?;
        view.hydrate_private_models(&mut state)?;
        let control = state
            .controls
            .get(&binding.task_id)
            .ok_or("native MODEL control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("native MODEL run ended")?;
        let frozen = control
            .model_authority
            .as_ref()
            .ok_or("native MODEL conscious start approval absent")?;
        let expected_epoch = binding.epoch;
        if run.epoch != expected_epoch
            || control.run_epoch != expected_epoch
            || frozen.epoch != expected_epoch
            || run.agent_id != binding.agent_id
            || control
                .directions
                .last()
                .map_or(0, |direction| direction.revision)
                != binding.revision
            || run.approval.as_ref().is_none_or(|approval| {
                approval.person != frozen.starter.person
                    || binding.starter.as_deref() != Some(approval.person.as_str())
            })
        {
            return Err("native MODEL approved run identity changed".into());
        }
        let contract: Self =
            serde_json::from_value(frozen.contract.clone()).map_err(|e| e.to_string())?;
        contract.source_for(&view.record.room.to_string(), &binding.task_id)?;
        if contract.request_model != frozen.request_model || contract.kind != frozen.kind {
            return Err("native MODEL approved contract changed".into());
        }
        Ok(contract)
    }

    /// The genuine semantic task resource address used by the broker provider.
    pub fn task_resource(room: &str, task_id: &str) -> TreePath {
        TreePath::room(room)
            .app(tilo_desktop_model::task::TASK_PROVIDER)
            .kind(tilo_desktop_model::task::TASK_KIND)
            .resource(format!("desktop:{task_id}"))
    }

    pub fn for_task(
        room: &str,
        task_id: &str,
        kind: ActionKind,
        selection: FrozenNativeModelSelection,
    ) -> Self {
        let resource = Self::task_resource(room, task_id);
        Self {
            request_model: resource.clone().action("requestModel"),
            window: resource,
            kind,
            selection,
        }
    }

    /// Resolve the closed source against the actual delivered task identity.
    /// Legacy window constructors remain valid without adding a struct field.
    /// # Errors
    /// Returns an error if current authority, run binding or durable custody cannot be verified.
    pub fn source_for(&self, room: &str, task_id: &str) -> Result<NativeModelSource, String> {
        if self.window.room_id() != Some(room)
            || self.window.is_root()
            || self
                .window
                .segments()
                .iter()
                .any(|segment| segment.id().is_none())
            || self.request_model != self.window.clone().action("requestModel")
        {
            return Err("native MODEL source requestModel contract invalid".into());
        }
        if self.window == Self::task_resource(room, task_id) {
            return Ok(NativeModelSource::Task {
                task_id: task_id.into(),
                resource: self.window.clone(),
            });
        }
        // A governance resource belonging to another task is never a Window.
        let governance = TreePath::room(room)
            .app(tilo_desktop_model::task::TASK_PROVIDER)
            .kind(tilo_desktop_model::task::TASK_KIND);
        if governance.covers(&self.window) {
            return Err("native MODEL task source identity changed".into());
        }
        Ok(NativeModelSource::Window {
            window: self.window.clone(),
        })
    }
}

/// Trusted shell hook consulted only inside the approved start transaction.
/// It must snapshot the entire selected nonsecret profile/model and resolve
/// the actual host source, never accept those values from invocation JSON.
pub trait NativeModelStartAuthority {
    /// # Errors
    /// Returns an error if current authority, run binding or durable custody cannot be verified.
    fn approved_model_start(
        &self,
        view: &NativeCommittedView,
        task_id: &str,
        person: &tilo_user_identity::DeviceCert,
        now: SystemTime,
    ) -> Result<NativeModelContract, String>;
}

/// A private, exact MODEL requirement. Request digests cover final outbound bytes,
/// not a prompt or an earlier provider-neutral request before HTTP preparation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedNativeModelRequirement {
    pub room: RoomId,
    pub run: crate::task_policy::NativeTaskBinding,
    pub invocation: uuid::Uuid,
    pub request: uuid::Uuid,
    pub host: [u8; 32],
    pub private_run_digest: [u8; 32],
    pub modelselection: NativeModelContract,
    pub requestdigest: [u8; 32],
}

/// Closed planning branch read from the authenticated delivered lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeModelPlanning {
    Manual,
    RulesOnly,
    Model,
}

/// Host-owned connection and lease evidence, refreshed inside the owner turn.
/// Never deserialize this from the calling app's request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeModelContext {
    pub actor: String,
    pub task: String,
    pub epoch: u64,
    pub revision: u64,
    pub planning: NativeModelPlanning,
    pub private_run_digest: [u8; 32],
    pub contract: NativeModelContract,
}

/// Protected gateway connection adapter. Implementations must resolve the actual
/// launch-authenticated actor, live leased run and current host-published contract
/// from host custody. They must refuse disconnected/replaced connections. This
/// callback runs while owner mutation is locked: it must not reenter the owner,
/// block on a queue or load secrets asynchronously.
pub trait NativeModelAuthority {
    /// Pin connection, lease and configuration through the synchronous transfer.
    /// The view is already under the owner turn; implementations must not reenter it.
    /// # Errors
    /// Returns an error if current authority, run binding or durable custody cannot be verified.
    fn with_current_model_context(
        &self,
        _view: &NativeCommittedView,
        _now: SystemTime,
        _enqueue: &mut dyn FnMut(NativeModelContext) -> Result<(), String>,
    ) -> Result<(), String> {
        Err("native MODEL pinned authority unavailable".into())
    }

    /// # Errors
    /// Missing or changed authenticated connection, lease or selection.
    fn current_model_context(
        &self,
        view: &NativeCommittedView,
        now: SystemTime,
    ) -> Result<NativeModelContext, String>;
}

fn digest(value: &impl Serialize) -> Result<[u8; 32], String> {
    Ok(*blake3::hash(&serde_json::to_vec(value).map_err(|error| error.to_string())?).as_bytes())
}

impl TypedNativeModelRequirement {
    /// Freeze final bytes after all provider-specific preparation and waits.
    /// # Errors
    /// Incomplete identities or a non-object selection contract.
    pub fn freeze(mut requirement: Self, outbound_body: &[u8]) -> Result<Self, String> {
        requirement.requestdigest = *blake3::hash(outbound_body).as_bytes();
        requirement.binding()?;
        Ok(requirement)
    }

    /// Internal ledger metadata only: action/resource stay empty. The typed wire
    /// command and domain-separated ceiling distinguish MODEL custody; neither
    /// invents a contract in the application-action registry.
    /// # Errors
    /// Incomplete frozen metadata or serialization failure.
    pub fn binding(&self) -> Result<NativeInvocationBinding, String> {
        let selection = &self.modelselection.selection;
        if self.invocation.is_nil()
            || self.request.is_nil()
            || self.run.task_id.is_empty()
            || self.run.agent_id.is_empty()
            || self.run.starter.as_deref().is_none_or(str::is_empty)
            || selection.app.is_empty()
            || selection.provider.is_empty()
            || selection.model.is_empty()
            || !selection.contract.is_object()
            || self.private_run_digest == [0; 32]
            || self.requestdigest == [0; 32]
        {
            return Err("native MODEL frozen metadata incomplete".into());
        }
        Ok(NativeInvocationBinding {
            room: self.room,
            task: self.run.task_id.clone(),
            agent: self.run.agent_id.clone(),
            epoch: self.run.epoch,
            revision: self.run.revision,
            invocation: self.invocation,
            request: self.request,
            host: self.host,
            app: selection.app.clone(),
            provider: selection.provider.clone(),
            resource: String::new(),
            action: String::new(),
            arguments_digest: self.requestdigest,
            contract_digest: digest(&self.modelselection)?,
            ceiling_digest: digest(&("tilo-native-model-v1", self))?,
            private_run_digest: self.private_run_digest,
        })
    }

    /// Check every ceiling on the same actual actor. Ask is not unattended MODEL
    /// authority and cannot borrow an action confirmation or another actor's Allow.
    /// # Errors
    /// Any exact binding, authenticated lease, route or live permission changed.
    pub fn check(
        &self,
        view: &NativeCommittedView,
        exact: &NativeInvocationBinding,
        authority: &impl NativeModelAuthority,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.binding()? != *exact {
            return Err("native MODEL exact binding changed".into());
        }
        if view.device() == view.governor()
            && NativeWorkspaceOwner::current_private_run_digest(view, &self.run)?
                != self.private_run_digest
        {
            return Err("native MODEL private owner envelope changed".into());
        }
        let current = authority.current_model_context(view, now)?;
        let state = tilo_peer_state::workspace_task::snapshot(
            view.peer()
                .admitted_room()
                .ok_or("native MODEL room absent")?,
        )?;
        self.check_registered_context(view.peer(), &state, exact, &current, now)
    }

    /// Recheck policy using independently registered physical-host context.
    /// The governor must also check its private run digest and registration ledger.
    /// # Errors
    /// Any exact binding, run frontier or policy ceiling changed.
    pub fn check_registered_context(
        &self,
        peer: &crate::StatePeer,
        state: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
        exact: &NativeInvocationBinding,
        current: &NativeModelContext,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.binding()? != *exact || peer.room() != self.room.to_string() {
            return Err("native MODEL exact room or binding changed".into());
        }
        if current.actor != self.run.agent_id
            || current.task != self.run.task_id
            || current.epoch != self.run.epoch
            || current.revision != self.run.revision
            || current.planning != NativeModelPlanning::Model
            || current.private_run_digest != self.private_run_digest
            || current.contract != self.modelselection
        {
            return Err("native MODEL actor, planning or selection changed".into());
        }
        let doc = peer.admitted_room().ok_or("native MODEL room absent")?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.id == self.run.task_id)
            .ok_or("native MODEL task absent")?;
        let control = state
            .controls
            .get(&self.run.task_id)
            .ok_or("native MODEL control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("native MODEL run ended")?;
        if task.route != tilo_desktop_model::WorkspaceTaskRoute::Model
            || task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
            || task.assigned_agent.as_deref() != Some(current.actor.as_str())
            || run.agent_id != current.actor
            || run.epoch != self.run.epoch
            || control.run_epoch != self.run.epoch
            || control
                .directions
                .last()
                .map_or(0, |direction| direction.revision)
                != self.run.revision
        {
            return Err("native MODEL route or run frontier changed".into());
        }
        let approval = run.approval.as_ref().ok_or("native MODEL starter absent")?;
        if self.run.starter.as_deref() != Some(approval.person.as_str()) {
            return Err("native MODEL starter changed".into());
        }
        peer.workspace()
            .check_task_execution_actor(&current.actor, Some(approval), now)?;
        let room = self.room.to_string();
        self.modelselection.source_for(&room, &self.run.task_id)?;
        let node = &self.modelselection.request_model;
        let policy = tilo_desktop_model::projection::room_policy_of(doc, &room);
        let cx = Context::at_time(now);
        let kind = self.modelselection.kind;
        let delegation = roles::agent_delegation(&room, &current.actor, &approval.person);
        let agent = Authority::person(delegation.party(&policy.grants)).decide(node, kind, &cx);
        let starter = peer.permissions(approval.device, now)?;
        let host = peer.permissions(self.host, now)?;
        let frozen = control
            .model_authority
            .as_ref()
            .ok_or("native MODEL separately approved authority absent")?;
        if starter.person != approval.person
            || agent.answer != Answer::Allow
            || !starter.allows(node, kind, now)
            || !host.allows(node, kind, now)
            || frozen.epoch != self.run.epoch
            || frozen.request_model != *node
            || frozen.kind != kind
            || tilo_peer_state::workspace_task::private_model_contract_reference(&frozen.contract)?
                != tilo_peer_state::workspace_task::private_model_contract_reference(
                    &serde_json::to_value(&self.modelselection).map_err(|e| e.to_string())?,
                )?
            || frozen.starter.person != approval.person
            || frozen.starter.answer(node, kind, &cx) != Answer::Allow
            || policy
                .task_answer(&self.run.task_id, node, kind, &cx)
                .unwrap_or(Answer::Allow)
                != Answer::Allow
        {
            return Err("native MODEL starter, agent, host or task ceiling denies dispatch".into());
        }
        Ok(())
    }
}

impl NativeWorkspaceOwner {
    /// Persist MODEL reservation after typed policy, never an application action.
    /// # Errors
    /// Changed policy, Stop, stale binding or persistence failure.
    pub fn reserve_native_model(
        &self,
        requirement: &TypedNativeModelRequirement,
        custody: NativeModelGovernorCustody<'_>,
        authority: &impl NativeModelAuthority,
    ) -> Result<NativeInvocationReservation, String> {
        self.reserve_native_invocation(
            requirement.binding()?,
            custody.host,
            custody.key,
            custody.now,
            |view, exact| requirement.check(view, exact, authority, custody.now),
        )
    }

    /// Recheck typed MODEL policy and take exact governor dispatch custody.
    /// # Errors
    /// Changed policy, Stop, replay or persistence failure.
    pub fn claim_native_model(
        &self,
        requirement: &TypedNativeModelRequirement,
        reservation: &NativeInvocationReservation,
        custody: NativeModelGovernorCustody<'_>,
        authority: &impl NativeModelAuthority,
    ) -> Result<NativeInvocationPermit, String> {
        self.claim_native_invocation(
            reservation,
            custody.host,
            custody.key,
            custody.now,
            |view, exact| requirement.check(view, exact, authority, custody.now),
        )
    }
}

/// Governor-only authenticated host evidence and signer; never sent to the app.
#[derive(Clone, Copy)]
pub struct NativeModelGovernorCustody<'a> {
    pub host: &'a DeviceCert,
    pub key: &'a SigningKey,
    pub now: SystemTime,
}

/// Same-process gate used by the actual HTTP gateway, not a boolean permission
/// RPC. Queue transfer runs under the owner's dispatch/fence mutation turn, after
/// signed permit consumption and durable intent. No future send token is returned.
pub struct HostDispatchModel<'a, A> {
    pub owner: &'a NativeWorkspaceOwner,
    pub authority: &'a A,
}

impl<A: NativeModelAuthority> HostDispatchModel<'_, A> {
    fn enqueue_current<T>(
        &self,
        view: &NativeCommittedView,
        requirement: &TypedNativeModelRequirement,
        enqueue: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        struct Pinned(NativeModelContext);
        impl NativeModelAuthority for Pinned {
            fn current_model_context(
                &self,
                _: &NativeCommittedView,
                _: SystemTime,
            ) -> Result<NativeModelContext, String> {
                Ok(self.0.clone())
            }
        }
        let mut enqueue = Some(enqueue);
        let mut result = None;
        self.authority
            .with_current_model_context(view, SystemTime::now(), &mut |current| {
                // Fresh wall clock after publication and after acquiring every gate.
                requirement.check(
                    view,
                    &requirement.binding()?,
                    &Pinned(current),
                    SystemTime::now(),
                )?;
                let send = enqueue.take().ok_or("native MODEL duplicate enqueue")?;
                result = Some(send()?);
                Ok(())
            })?;
        result.ok_or_else(|| "native MODEL enqueue absent".into())
    }

    /// Local governor/physical-host admission, with no externally exposed
    /// intermediate reservation or permit. Returns the consumed signed permit
    /// for the actual successful-response delivery receipt, never for replay.
    /// # Errors
    /// Foreign host, changed request or authority, Stop, replay or publication failure.
    pub fn dispatch_local<T>(
        &self,
        requirement: &TypedNativeModelRequirement,
        custody: NativeModelGovernorCustody<'_>,
        outbound_body: &[u8],
        enqueue: impl FnOnce(&[u8]) -> Result<T, String>,
    ) -> Result<(T, NativeInvocationPermit), String> {
        if *blake3::hash(outbound_body).as_bytes() != requirement.requestdigest {
            return Err("native MODEL final HTTP body changed".into());
        }
        self.owner.admit_local_native_invocation_with_view(
            requirement.binding()?,
            custody.host,
            custody.key,
            |view, exact, now| requirement.check(view, exact, self.authority, now),
            |view| self.enqueue_current(view, requirement, || enqueue(outbound_body)),
        )
    }

    /// Transfer this exact prepared HTTP attempt into the host transport queue.
    /// `enqueue` must perform the irrevocable transfer synchronously, not return
    /// an unpolled future or schedule a later admission check. Configuration waits
    /// and retry backoff precede this call; every new HTTP attempt needs new custody.
    /// A callback failure remains durably in-flight/unknown: never auto-replay it.
    /// # Errors
    /// Changed body, selection, policy, consumed permit, host fence or storage failure.
    pub fn dispatch<T>(
        &self,
        requirement: &TypedNativeModelRequirement,
        permit: &NativeInvocationPermit,
        attempt: NativeModelHttpAttempt<'_>,
        enqueue: impl FnOnce(&[u8]) -> Result<T, String>,
    ) -> Result<T, String> {
        if *blake3::hash(attempt.outbound_body).as_bytes() != requirement.requestdigest {
            return Err("native MODEL final HTTP body changed".into());
        }
        let exact = requirement.binding()?;
        self.owner.admit_reserved_native_invocation_with_view(
            permit,
            &exact,
            attempt.governor,
            |view, now| requirement.check(view, &exact, self.authority, now),
            |view| self.enqueue_current(view, requirement, || enqueue(attempt.outbound_body)),
        )
    }
}

/// Exact final bytes in host custody and authenticated source of the permit.
#[derive(Clone, Copy)]
pub struct NativeModelHttpAttempt<'a> {
    pub governor: &'a DeviceCert,
    pub outbound_body: &'a [u8],
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn selection() -> FrozenNativeModelSelection {
        FrozenNativeModelSelection {
            app: "so.tilo.agent".into(),
            provider: "local".into(),
            model: "selected".into(),
            contract: serde_json::json!({"endpoint":"http://localhost:8000", "enabled":true}),
        }
    }

    #[test]
    fn task_contract_names_the_actual_windowless_resource_and_exact_task() {
        let contract =
            NativeModelContract::for_task("room", "task:created", ActionKind::Outside, selection());
        assert_eq!(
            contract.source_for("room", "task:created").unwrap(),
            NativeModelSource::Task {
                task_id: "task:created".into(),
                resource: TreePath::room("room")
                    .app("so.tilo.desktop")
                    .kind("workspace-task")
                    .resource("desktop:task:created"),
            }
        );
        assert!(contract.source_for("room", "task:another").is_err());
        assert!(contract.source_for("another-room", "task:created").is_err());
        let mut forged = contract;
        forged.request_model = TreePath::room("room").action("requestModel");
        assert!(forged.source_for("room", "task:created").is_err());
    }

    #[test]
    fn existing_window_struct_constructor_remains_a_window_source() {
        let window = TreePath::room("room")
            .app("chat")
            .kind("window")
            .resource("actual-window");
        let contract = NativeModelContract {
            request_model: window.clone().action("requestModel"),
            window: window.clone(),
            kind: ActionKind::Outside,
            selection: selection(),
        };
        assert_eq!(
            contract.source_for("room", "task:created").unwrap(),
            NativeModelSource::Window { window }
        );
    }
}
