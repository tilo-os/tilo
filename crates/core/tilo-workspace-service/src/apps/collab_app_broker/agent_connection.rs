//! One agent session's api to the broker: what its steps do in turn, as the session's caller.
//!
//! It is served as any admitted connection is ([`ConnectionActionProvider`]): the workspace's
//! grants decide each step, the agent capped at what its person may do and at what a device lets
//! its agents do, and only while the session goes ([`AppBrokerState::turn_session_goes`]).
//!
//! A step the grants ask first about is put to the person on the host's sheet
//! ([`crate::apps::app_actions::AppActionConsentPrompter`]), the same sheet apps' questions use:
//!
//! - while the sheet is up the caller waits; the caller leaving, the session ending or the person
//!   dismissing the sheet withdraws the question, and the caller hears why;
//! - the person allowing it answers with a token naming that one invocation, which the caller
//!   invokes again with; "allow for this agent" also keeps a grant on the session for its later
//!   steps like it;
//! - the person declining refuses it, in the words every declined step is refused in.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, pending,
};

use super::AppBrokerState;
use super::action_provider::ConnectionActionProvider;
use super::published_invocation::InvocationCaller;
use crate::apps::app_actions::{AGENT_SESSION_ASK, AppActionConsentPrompter};

/// How often a waiting question looks whether its session still goes.
const STILL_GOES_EVERY: Duration = Duration::from_millis(500);

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

impl AppBrokerState {
    fn consent_prompter(&self) -> Option<Arc<dyn AppActionConsentPrompter>> {
        self.app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())
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

    /// Put `request` to the session's person on the host's sheet and wait for their answer: the
    /// confirmation the caller invokes it again with, or why not.
    async fn ask_person(
        &self,
        request: &InvokeRequest,
    ) -> Result<ConfirmedInvocation, ActionCallError> {
        let state = Arc::clone(self.state());
        let caller = &self.0.caller;
        let person = caller
            .acting_for_person()
            .ok_or(ActionCallError::NotPermitted)?
            .to_owned();
        let question = state
            .approval_question(caller, &person, request)?
            .ok_or(ActionCallError::UnknownAction)?;
        let nobody = || ActionCallError::Unavailable {
            reason: "nobody is at this desktop to answer the agent's question".into(),
        };
        let prompter = state.consent_prompter().ok_or_else(nobody)?;
        let mut asked = question.request.clone();
        asked.handle = format!("{AGENT_SESSION_ASK}{}", request.invocation.0);
        asked.agent_session = Some(self.0.session.clone());
        let label = asked.action_label.clone();
        let handle = asked.handle.clone();
        let answer = prompter.ask(asked).ok_or_else(nobody)?;
        let mut asking = Asking {
            prompter,
            handle,
            answered: false,
        };
        let answer = self.answer_to(answer, &label).await?;
        asking.answered = true;
        if matches!(
            answer.lifetime,
            Some(
                tilo_action_policy::Lifetime::Session | tilo_action_policy::Lifetime::UntilRevoked
            )
        ) && let (Some(grant), Some(sessions)) = (
            question.answer.clone(),
            state.services.agent_sessions.as_ref(),
        ) {
            sessions.keep_answer(&self.0.session, grant);
        }
        state.approval_answer(&question, request, &answer.edits)
    }

    /// The person's answer on the sheet to the question about `label`, waited for while the
    /// session goes: why not, where it ended or the person dismissed it.
    async fn answer_to(
        &self,
        mut answer: tokio::sync::oneshot::Receiver<crate::apps::app_actions::AppActionAnswer>,
        label: &str,
    ) -> Result<crate::apps::app_actions::AppActionAnswer, ActionCallError> {
        let refused = |reason: String| ActionCallError::Refused { reason };
        let answer = loop {
            tokio::select! {
                answer = &mut answer => break answer,
                () = tilo_rt::sleep(STILL_GOES_EVERY) => {
                    if let Err(reason) = self.goes() {
                        return Err(refused(format!(
                            "the question about {label} was withdrawn: {reason}"
                        )));
                    }
                }
            }
        };
        let Ok(answer) = answer else {
            return Err(refused(format!(
                "the question about {label} was withdrawn: the person dismissed it"
            )));
        };
        if !answer.allowed() {
            return Err(refused(format!(
                "{}: the person declined {label}",
                tilo_actions::draft::DECLINED
            )));
        }
        Ok(answer)
    }
}

/// A question on the sheet, withdrawn unless it was answered.
struct Asking {
    prompter: Arc<dyn AppActionConsentPrompter>,
    handle: String,
    answered: bool,
}

impl Drop for Asking {
    fn drop(&mut self) {
        if !self.answered {
            self.prompter.withdraw(&self.handle);
        }
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
