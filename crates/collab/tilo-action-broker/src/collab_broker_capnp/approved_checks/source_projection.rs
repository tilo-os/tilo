//! Bounded child reads retain the original Person or accepted native Agent admission.
use super::ApprovedCheckContext;
use crate::collab_broker_capnp::{
    CapnpBrokerState, PublishedAction, PublishedSurface, enrolled_tasks,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_actions::{ExecutionMode, InvocationUpdate, PermissionRequirement, ResultSpec};

/// Trusted host selector; these inert names never substitute for current admission.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBinding {
    pub app_id: String,
    pub workspace: String,
    pub record: String,
    pub resource_identity: tilo_actions::ResourceSemanticIdentity,
    pub action: String,
}
/// Exact observed child terminal and target facts; no portable authority is asserted.
#[derive(Serialize)]
pub struct SourceObservation {
    pub invocation: String,
    pub parent_invocation: String,
    pub binding: SourceBinding,
    pub output: serde_json::Value,
    #[serde(skip)]
    guard: SourceGuard,
}
enum ReadOrigin {
    Agent(Box<enrolled_tasks::SourceOrigin>),
    Person {
        state: Arc<CapnpBrokerState>,
        caller: Box<crate::collab_broker_capnp::InvocationCaller>,
        context: Box<tilo_actions::ActionContext>,
    },
}
impl ReadOrigin {
    fn retained(
        state: Arc<CapnpBrokerState>,
        caller: crate::collab_broker_capnp::InvocationCaller,
        context: tilo_actions::ActionContext,
    ) -> Result<Self, String> {
        if let crate::collab_broker_capnp::InvocationCaller::Local { admitted, .. } = &caller
            && admitted.agent_id.is_some()
        {
            return enrolled_tasks::SourceOrigin::retained(state, caller, &context)
                .map(|origin| Self::Agent(Box::new(origin)));
        }
        let origin = Self::Person {
            state,
            caller: Box::new(caller),
            context: Box::new(context),
        };
        origin.actor()?;
        Ok(origin)
    }
    fn actor(&self) -> Result<crate::collab_broker_capnp::BrokerActor, String> {
        match self {
            Self::Agent(origin) => {
                origin.recheck()?;
                Ok(origin.actor())
            }
            Self::Person { state, caller, .. } => {
                let admitted = match caller.as_ref() {
                    crate::collab_broker_capnp::InvocationCaller::NativePerson {
                        room,
                        certificate,
                        time,
                        ..
                    } => {
                        state.checked_policy_time(Some(time))?;
                        state.current_native_person(room, certificate)?;
                        return Ok(state.certified_person(room, certificate));
                    }
                    crate::collab_broker_capnp::InvocationCaller::Certified { caller, .. } => {
                        state.current_action_caller(caller)?
                    }
                    crate::collab_broker_capnp::InvocationCaller::Local { route, admitted } => {
                        state
                            .revalidate_admission(route, admitted)
                            .map_err(|error| error.to_string())?
                    }
                };
                if admitted.agent_id.is_some()
                    || admitted.host_scope.is_some()
                    || admitted.worker_registration.is_some()
                    || admitted
                        .certificate
                        .as_ref()
                        .is_none_or(|certificate| certificate.capabilities.can_act_as_host)
                {
                    return Err(
                        "source reads require a current Person or accepted native Agent origin"
                            .into(),
                    );
                }
                Ok(admitted.broker_actor())
            }
        }
    }
    fn recheck(&self) -> Result<(), String> {
        self.actor().map(|_| ())
    }
    fn start(
        &self,
        target: runtime_collab::TaskAction<'_>,
    ) -> Result<crate::collab_broker_capnp::InvocationJob, String> {
        match self {
            Self::Agent(origin) => origin.start(target),
            Self::Person {
                state,
                caller,
                context,
            } => {
                let actor = self.actor()?;
                state.start_invocation_confirmed_for_origin(
                    target.peer_id,
                    target.resource,
                    target.action,
                    crate::collab_broker_capnp::InvocationInputs {
                        requested: None,
                        caller: None,
                        execution: None,
                        argument: target.argument,
                        actor: &actor,
                        confirmation_token: None,
                        context: Some(context.as_ref().clone()),
                    },
                    Some(caller.as_ref().clone()),
                )
            }
        }
    }
}
struct SourceGuard {
    context: ApprovedCheckContext,
    state: Arc<CapnpBrokerState>,
    origin: ReadOrigin,
    binding: SourceBinding,
    peer: u64,
    action: PublishedAction,
}
impl SourceGuard {
    fn recheck(&self) -> Result<(), String> {
        self.context.recheck()?;
        self.origin.recheck()?;
        let (current, action) = selected(&self.state, &self.binding)?;
        if current.peer_id != self.peer || action != self.action {
            return Err("source connection or operation changed during observation".into());
        }
        let seconds = self
            .state
            .checked_policy_time(Some(self.context.original.policy_time()))?
            .1
            / 1_000_000_000;
        self.state.authorize_invocation_permission(
            &self.binding.workspace,
            &self.binding.app_id,
            &self.action.resource,
            &self.binding.action,
            PermissionRequirement::Read,
            &self.origin.actor()?,
            seconds,
        )
    }
}
impl SourceObservation {
    /// Recheck the original parent, caller custody and independent source read authority.
    ///
    /// # Errors
    /// Refuses cancellation, retirement, replaced sources and revoked read permission.
    pub fn recheck(&self) -> Result<(), String> {
        self.guard.recheck()
    }
}
fn selected(
    state: &CapnpBrokerState,
    binding: &SourceBinding,
) -> Result<(PublishedSurface, PublishedAction), String> {
    if !binding.resource_identity.is_valid()
        || binding.app_id.is_empty()
        || binding.app_id.len() > 128
        || binding.workspace.is_empty()
        || binding.workspace.len() > 128
        || binding.record.len() > 1024
        || binding.action.is_empty()
        || binding.action.len() > 128
    {
        return Err("source binding malformed or oversized".into());
    }
    let surfaces = state
        .surfaces
        .lock()
        .map_err(|_| "source publication unavailable")?;
    let mut matching = surfaces
        .values()
        .filter(|surface| {
            surface.live
                && surface.app_id == binding.app_id
                && surface.route.cluster_id == binding.workspace
                && surface.route.record_id == binding.record
        })
        .flat_map(|surface| {
            surface
                .actions
                .iter()
                .filter(|action| {
                    action.semantic_identity.as_ref() == Some(&binding.resource_identity)
                        && action.action == binding.action
                })
                .map(|action| (surface.clone(), action.clone()))
        });
    let pair = matching
        .next()
        .ok_or("exact source read is not currently published")?;
    if matching.next().is_some() {
        return Err("source publication is ambiguous".into());
    }
    drop(surfaces);
    if pair.1.contract.permission != PermissionRequirement::Read
        || pair.1.contract.execution != ExecutionMode::Immediate
        || !pair.1.contract.reads_document()
        || !matches!(pair.1.contract.result, ResultSpec::Json { .. })
    {
        return Err("source operation is not an immediate typed document read".into());
    }
    Ok(pair)
}
impl ApprovedCheckContext {
    fn source_origin(&self, state: &Arc<CapnpBrokerState>) -> Result<ReadOrigin, String> {
        self.recheck()?;
        let (caller, context) = self.original.source_origin(state)?;
        ReadOrigin::retained(state.clone(), caller, context)
    }
    /// Read through the normal published action with the original caller's current authority.
    ///
    /// # Errors
    /// Refuses changed/foreign records, missing read permission, cancellation, retirement,
    /// oversized results and uncertain terminal outcomes. A timeout never returns a snapshot.
    pub fn read_source(
        &self,
        binding: &SourceBinding,
        argument: &serde_json::Value,
    ) -> Result<SourceObservation, String> {
        let state = self.state.upgrade().ok_or("source broker retired")?;
        if binding.workspace != self.workspace() {
            return Err("source binding is outside the original task workspace".into());
        }
        let arguments = argument
            .as_object()
            .ok_or("source read requires a typed argument object")?;
        let encoded = tilo_actions::remote::InvokeArgument::Json(arguments.clone())
            .to_slot()
            .ok_or("source read argument slot unavailable")?;
        if encoded.len() > 8192 {
            return Err("source read arguments exceed bound".into());
        }
        let origin = self.source_origin(&state)?;
        let (surface, action) = selected(&state, binding)?;
        let job = origin.start(runtime_collab::TaskAction {
            peer_id: surface.peer_id,
            resource: &action.resource,
            action: &action.action,
            argument: Some(&encoded),
        })?;
        let guard = SourceGuard {
            context: self.clone(),
            state: state.clone(),
            origin,
            binding: binding.clone(),
            peer: surface.peer_id,
            action,
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .ok_or("source deadline overflow")?;
        loop {
            guard.recheck()?;
            match job.completion.recv_timeout(Duration::from_millis(50)) {
                Ok(_) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => return Err("source read terminal outcome unobserved".into()),
            }
        }
        guard.recheck()?;
        let (invocation, output) = match state.invocation_status(job.id) {
            Some(InvocationUpdate::Succeeded { invocation, output }) => (invocation, output),
            Some(InvocationUpdate::Failed { error, .. }) => {
                let code: String = error.code.chars().take(128).collect();
                return Err(format!("source read terminal failed: {code}"));
            }
            _ => return Err("source read did not produce an observed successful terminal".into()),
        };
        let output = output
            .value
            .ok_or("source read terminal has no typed value")?;
        if serde_json::to_vec(&output)
            .map_err(|error| error.to_string())?
            .len()
            > 32768
        {
            return Err("source read terminal exceeds bound".into());
        }
        Ok(SourceObservation {
            invocation: invocation.0.to_string(),
            parent_invocation: self.invocation(),
            binding: binding.clone(),
            output,
            guard,
        })
    }
}
