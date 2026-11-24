//! One agent session's api to the broker: what its steps do in turn, as the session's caller.
//!
//! It is served as any admitted connection is ([`ConnectionActionProvider`]): the workspace's
//! grants decide each step, the agent capped at what its person may do and at what a device lets
//! its agents do, and only while the session goes ([`AppBrokerState::turn_session_goes`]).
//!
//! A step the grants ask first about waits as the person's asked record, answered by them on one
//! of their own devices, as every other call that asks first does.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};

use super::AppBrokerState;
use super::action_provider::ConnectionActionProvider;
use super::published_invocation::InvocationCaller;

struct Inner {
    provider: ConnectionActionProvider,
    caller: Caller,
    workspace: String,
    agent: String,
    session: String,
}

/// One admitted agent session's connection.
#[derive(Clone)]
pub(super) struct AgentConnection(Arc<Inner>);

impl std::fmt::Debug for AgentConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConnection")
            .field("agent", &self.0.agent)
            .field("session", &self.0.session)
            .finish_non_exhaustive()
    }
}

impl AppBrokerState {
    /// The API `caller`, an agent in an agent session, takes its steps through, on a connection
    /// admitted as `origin`. `None` for a caller in no agent session.
    pub(super) fn agent_session_api(
        self: &Arc<Self>,
        caller: &Caller,
        origin: InvocationCaller,
    ) -> Option<AgentConnection> {
        let acting = caller.acting();
        let (session, agent) = acting.agent_session.as_ref().zip(acting.agent_id())?;
        let workspace = caller.workspace()?.to_owned();
        Some(AgentConnection(Arc::new(Inner {
            provider: ConnectionActionProvider {
                state: Arc::clone(self),
                own_peer: self.next_peer_id(),
                origin,
            },
            caller: caller.clone(),
            workspace,
            agent: agent.to_owned(),
            session: session.as_str().to_owned(),
        })))
    }
}

impl AgentConnection {
    fn state(&self) -> &Arc<AppBrokerState> {
        &self.0.provider.state
    }

    /// Whether the session still goes: the connection's authority ends with it.
    fn goes(&self) -> Result<(), String> {
        self.state()
            .turn_session_goes(&self.0.workspace, &self.0.agent, &self.0.session)
            .map(drop)
    }

    /// The caller is one of this connection's, still live.
    fn check(&self, caller: &Caller) -> Result<(), ActionCallError> {
        if !caller.execution_is_live() || !self.0.caller.execution_is_live() {
            return Err(ActionCallError::NotPermitted);
        }
        self.goes()
            .map_err(|reason| ActionCallError::Unavailable { reason })
    }
}

/// The records an agent session reads and changes outside their apps (its programs' `find`,
/// `read` and every action a program calls), as the session's own caller, while it goes: the
/// broker's record api, decided by the workspace's policy for the session's agent and person as
/// every other step of the session is. Without it a turn's programs ran no change at all ("this
/// desktop applies no task's changes at a record's origin").
struct SessionRecords {
    connection: AgentConnection,
    reads: Option<Arc<dyn tilo_actions::remote::ResourceReads>>,
    changes: Option<Arc<dyn tilo_actions::remote::ResourceChanges>>,
}

impl tilo_actions::remote::ResourceReads for SessionRecords {
    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        self.connection.check(caller)?;
        self.reads
            .as_ref()
            .ok_or(ActionCallError::NotPermitted)?
            .read(&self.connection.0.caller, address)
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<tilo_actions::remote::RemoteActionMessage> {
        // Every reading is answered through `read`, which checks the session goes and reads as
        // its caller.
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        self.connection.check(caller)?;
        self.reads
            .as_ref()
            .ok_or(ActionCallError::NotPermitted)?
            .watch(&self.connection.0.caller, address, wake)
    }
}

impl tilo_actions::remote::ResourceChanges for SessionRecords {
    fn apply(
        &self,
        caller: &Caller,
        change: &tilo_actions::remote::ResourceChange,
    ) -> Result<serde_json::Value, ActionCallError> {
        self.connection.check(caller)?;
        self.changes
            .as_ref()
            .ok_or(ActionCallError::NotPermitted)?
            .apply(&self.connection.0.caller, change)
    }
}

impl AgentConnection {
    fn session_records(&self) -> SessionRecords {
        SessionRecords {
            connection: self.clone(),
            reads: self.state().records(),
            changes: self.state().resource_changes(),
        }
    }
}

impl ActionProvider for AgentConnection {
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        let records = self.session_records();
        records.reads.as_ref()?;
        Some(Arc::new(records))
    }

    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        let records = self.session_records();
        records.changes.as_ref()?;
        Some(Arc::new(records))
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        self.0.provider.query(&self.0.caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        // The session's group, whatever the client says: history lists the step under the
        // session, and taking its work back names it.
        let mut request = request;
        let mut context = request.context.take().unwrap_or_default();
        context.group = self.0.caller.agent_session();
        request.context = Some(context);
        self.0.provider.invoke(&self.0.caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.check(caller).ok()?;
        self.0.provider.observe(&self.0.caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.check(caller).ok()?;
        self.0.provider.next_change(&self.0.caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        self.0.provider.cancel(&self.0.caller, id)
    }
}
