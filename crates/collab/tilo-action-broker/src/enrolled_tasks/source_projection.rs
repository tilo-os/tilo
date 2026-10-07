//! Nested read admission retains the accepted Agent's current native task lease.
use super::{Context, task_actions};
use crate::collab_broker_capnp::{
    AdmittedJoinIdentity, CapnpBrokerState, InvocationCaller, InvocationJob,
};
use std::sync::Arc;

pub(in crate::collab_broker_capnp) struct SourceOrigin {
    context: Context,
    admitted: AdmittedJoinIdentity,
    numeric: u64,
    lease: String,
}
impl SourceOrigin {
    pub(in crate::collab_broker_capnp) fn retained(
        state: Arc<CapnpBrokerState>,
        caller: InvocationCaller,
        parent: &tilo_actions::ActionContext,
    ) -> Result<Self, String> {
        let InvocationCaller::Local { route, admitted } = caller else {
            return Err("source projection requires a retained native Agent caller".into());
        };
        let scope = admitted
            .managed_scope
            .ok_or("source caller has no enrolled custody")?;
        if admitted.agent_id.is_none() {
            return Err("source projection requires an accepted Agent".into());
        }
        let admitted = state
            .revalidate_admission(&route, &admitted)
            .map_err(|error| error.to_string())?;
        let connection = super::current(&state, scope)?;
        if connection.route != route {
            return Err("source caller route changed".into());
        }
        let peer = connection.peer.ok_or("source caller channel retired")?;
        let registry = state
            .enrolled_tasks
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        let mut matching = registry.runs.values().filter(|run| run.scope == scope);
        let run = matching.next().cloned().ok_or("source caller run absent")?;
        if matching.next().is_some() {
            return Err("source caller run is ambiguous".into());
        }
        drop(registry);
        let actual: tilo_actions::ActionContext =
            serde_json::from_value(run.envelope.task.action_context.clone())
                .map_err(|_| "retained source task context malformed")?;
        if actual.group.is_none()
            || parent.group != actual.group
            || parent.purpose != actual.purpose
        {
            return Err("source parent does not retain this exact accepted task context".into());
        }
        let result = Self {
            context: Context { state, scope, peer },
            admitted,
            numeric: run.envelope.task.run_id,
            lease: run.envelope.task.lease,
        };
        result.recheck()?;
        Ok(result)
    }
    pub(in crate::collab_broker_capnp) fn recheck(&self) -> Result<(), String> {
        let (_, cancelled) = task_actions::observed(&self.context, self.numeric, &self.lease)?;
        if cancelled {
            return Err("source projection task is cancelled".into());
        }
        let (_, connection) = self.context.connection()?;
        self.context
            .state
            .revalidate_admission(&connection.route, &self.admitted)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    pub(in crate::collab_broker_capnp) fn start(
        &self,
        target: runtime_collab::TaskAction<'_>,
    ) -> Result<InvocationJob, String> {
        self.recheck()?;
        task_actions::start_action(
            &self.context,
            &self.admitted,
            self.numeric,
            &self.lease,
            target,
        )
    }
    pub(in crate::collab_broker_capnp) fn actor(&self) -> crate::collab_broker_capnp::BrokerActor {
        self.admitted.broker_actor()
    }
}
