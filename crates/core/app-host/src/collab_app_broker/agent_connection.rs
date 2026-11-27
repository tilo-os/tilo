//! One agent session's api to the broker: what its steps do in turn, as the session's caller.
//!
//! It is served as any admitted connection is ([`ConnectionActionProvider`]): the workspace's
//! grants decide each step, the agent capped at what its person may do and at what a device lets
//! its agents do, and only while the session goes ([`AppBrokerState::turn_session_goes`]).
//!
//! A step the grants ask first about waits for the person as its asked record, and their
//! approve admits it again ([`tilo_actions::remote::InvokeRequest::approval`]).

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
    agent: tilo_action_policy::MemberId,
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
        let (session, agent) = acting
            .agent_session
            .as_ref()
            .zip(acting.member_id().filter(|member| member.is_agent()))?;
        let workspace = caller.workspace()?.to_owned();
        Some(AgentConnection(Arc::new(Inner {
            provider: ConnectionActionProvider {
                state: Arc::clone(self),
                own_peer: self.next_peer_id(),
                origin,
                serving: None,
            },
            caller: caller.clone(),
            workspace,
            agent: agent.clone(),
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

/// The records an agent session reads outside their apps (its programs' `find` and `read`), as
/// the session's own caller, while it goes: the workspace's record api, decided by the workspace's
/// policy for the session's agent and person as every other step of the session is.
struct SessionRecords {
    connection: AgentConnection,
    reads: Option<Arc<dyn tilo_actions::remote::ResourceReads>>,
}

impl tilo_actions::remote::ResourceReads for SessionRecords {
    // Every reading is answered through `read`, which checks the session goes and reads as its
    // caller.
    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer
    where
        Self: 'static,
    {
        Box::pin(tilo_action_calls::answer_read(
            self,
            caller.clone(),
            correlation,
            read,
        ))
    }

    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, tilo_actions::remote::ResourceCopy> {
        tilo_actions::remote::reads_answer(async move {
            self.connection.check(caller)?;
            self.reads
                .as_ref()
                .ok_or(ActionCallError::NotPermitted)?
                .read(&self.connection.0.caller, address)
                .await
        })
    }

    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
        wake: &'a tilo_actions::remote::ResourceWake,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        tilo_actions::remote::reads_answer(async move {
            self.connection.check(caller)?;
            self.reads
                .as_ref()
                .ok_or(ActionCallError::NotPermitted)?
                .watch(&self.connection.0.caller, address, wake)
                .await
        })
    }
}

impl AgentConnection {
    fn session_records(&self) -> SessionRecords {
        SessionRecords {
            connection: self.clone(),
            reads: self.state().records(),
        }
    }
}

impl ActionProvider for AgentConnection {
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        let records = self.session_records();
        records.reads.as_ref()?;
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

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        let mut request = request;
        let mut context = request.context.take().unwrap_or_default();
        context.group = self.0.caller.agent_session();
        request.context = Some(context);
        self.0.provider.run(&self.0.caller, request, admitted)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        if let Err(error) = self.check(caller) {
            return Some(Pending::ready(Err(error)));
        }
        let mut request = request.clone();
        let mut context = request.context.take().unwrap_or_default();
        context.group = self.0.caller.agent_session();
        request.context = Some(context);
        self.0
            .provider
            .asks_first(&self.0.caller, &request, admitted)
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        self.check(caller).map_err(|error| error.to_string())?;
        self.0.provider.watch(&self.0.caller, request)
    }
}
