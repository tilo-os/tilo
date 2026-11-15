//! One agent session's door to the broker: an MCP client the session's MCP server admitted, or an
//! agent answering a turn ([`super::turns`]).
//!
//! An MCP connection joins as the agent its certificate names, certified by a device of the person
//! who admitted it; a turn's agent acts as its connection was handed the turn. Either is then
//! served as any admitted connection is ([`ConnectionActionHost`]): the workspace's grants decide
//! each step, the agent capped at what its person may do and at what a device lets its agents do,
//! and only while the session goes ([`AppBrokerState::agent_session_goes`],
//! [`AppBrokerState::turn_session_goes`]).
//!
//! A step the grants ask first about is put to the person on the host's sheet
//! ([`crate::app_actions::AppActionConsentPrompter`]), the same sheet apps' questions use:
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
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, pending,
};
use tilo_peer_state::join::agent_cert::AgentCert;

use super::action_host::ConnectionActionHost;
use super::published_invocation::InvocationCaller;
use super::{AppBrokerState, CrdtRoute};
use crate::app_actions::{AGENT_SESSION_ASK, AppActionConsent, AppActionConsentPrompter};

/// How often a waiting question looks whether its session still goes.
const STILL_GOES_EVERY: Duration = Duration::from_millis(500);

struct Inner {
    host: ConnectionActionHost,
    caller: Caller,
    workspace: String,
    agent: String,
    session: String,
    /// The certificate an MCP connection joined with; `None` for a turn's agent.
    cert: Option<AgentCert>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // An MCP connection's execution ends with it; a turn's ends with the turn.
        if self.cert.is_some() {
            self.caller.retire_execution();
        }
    }
}

/// One admitted agent session's connection.
#[derive(Clone)]
pub struct AgentConnection(Arc<Inner>);

impl std::fmt::Debug for AgentConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConnection")
            .field("agent", &self.0.agent)
            .field("session", &self.0.session)
            .finish_non_exhaustive()
    }
}

impl AppBrokerState {
    /// Join `presentation`, an agent's (the device's certificate, the agent certificate it signed
    /// and the agent key's proof), as the agent session its certificate names, by the name
    /// `name` the person reads it by.
    ///
    /// # Errors
    /// The presentation does not verify, the workspace does not list the agent, or its session
    /// does not go.
    pub fn agent_connection(
        self: &Arc<Self>,
        presentation: &[u8],
        name: &str,
    ) -> Result<AgentConnection, String> {
        let decoded = runtime_collab::AgentJoinIdentity::decode(presentation)
            .map_err(|error| format!("the agent's presentation is malformed: {error}"))?
            .ok_or("an agent session joins only as a certified agent")?;
        let workspace = decoded.workspace_id;
        let route = CrdtRoute::record(
            tilo_actions::remote::task_programs::APP,
            workspace.clone(),
            "",
        );
        let admitted = super::verify_join_identity(
            &route,
            presentation,
            self.membership.as_ref(),
            self.trusted_devices.as_deref().map(Vec::as_slice),
            self.certificate_authority.get().map(Arc::as_ref),
        )?;
        let cert = admitted
            .agent_cert
            .as_deref()
            .cloned()
            .ok_or("agent certificate missing")?;
        self.admit_agent_join(&workspace, &cert.agent_id, &admitted)?;
        let caller = Caller::local_person(admitted.principal_id.clone())
            .in_workspace(workspace.clone())
            .delegated_to(cert.agent_id.clone())
            .in_new_execution()
            .named(name)
            .in_agent_session(cert.session.clone());
        let host = ConnectionActionHost {
            state: Arc::clone(self),
            own_peer: self.next_peer_id(),
            origin: InvocationCaller::Local {
                route,
                time: self.registered_policy_time()?,
                admitted: Box::new(admitted),
            },
        };
        Ok(AgentConnection(Arc::new(Inner {
            host,
            caller,
            workspace,
            agent: cert.agent_id.clone(),
            session: cert.session.clone(),
            cert: Some(cert),
        })))
    }

    /// The door `caller`, an agent in an agent session, takes its steps through, on a connection
    /// admitted as `origin`. `None` for a caller in no agent session.
    pub(super) fn agent_session_door(
        self: &Arc<Self>,
        caller: &Caller,
        origin: InvocationCaller,
    ) -> Option<AgentConnection> {
        let acting = caller.acting();
        let (session, agent) = acting.agent_session.as_ref().zip(acting.agent_id())?;
        let workspace = caller.workspace()?.to_owned();
        Some(AgentConnection(Arc::new(Inner {
            host: ConnectionActionHost {
                state: Arc::clone(self),
                own_peer: self.next_peer_id(),
                origin,
            },
            caller: caller.clone(),
            workspace,
            agent: agent.to_owned(),
            session: session.as_str().to_owned(),
            cert: None,
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
        &self.0.host.state
    }

    /// Whether the session still goes: the connection's authority ends with it.
    fn goes(&self) -> Result<(), String> {
        match &self.0.cert {
            Some(cert) => self.state().agent_session_goes(&self.0.workspace, cert),
            None => {
                self.state()
                    .turn_session_goes(&self.0.workspace, &self.0.agent, &self.0.session)
            }
        }
        .map(drop)
    }

    /// The caller is one of this connection's, still live.
    fn check(&self, caller: &Caller) -> Result<(), ActionHostError> {
        if !caller.execution_is_live() || !self.0.caller.execution_is_live() {
            return Err(ActionHostError::NotPermitted);
        }
        self.goes()
            .map_err(|reason| ActionHostError::Unavailable { reason })
    }

    /// Put `request` to the session's person on the host's sheet and wait for their answer: the
    /// confirmation the caller invokes it again with, or why not.
    async fn ask_person(
        &self,
        request: &InvokeRequest,
    ) -> Result<ConfirmedInvocation, ActionHostError> {
        let state = Arc::clone(self.state());
        let caller = &self.0.caller;
        let person = caller
            .acting_for_person()
            .ok_or(ActionHostError::NotPermitted)?
            .to_owned();
        let question = state
            .witness_question(caller, &person, request)?
            .ok_or(ActionHostError::UnknownAction)?;
        let nobody = || ActionHostError::Unavailable {
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
            answer.consent,
            AppActionConsent::Session | AppActionConsent::Always
        ) && let (Some(grant), Some(sessions)) = (
            question.answer.clone(),
            state.services.agent_sessions.as_ref(),
        ) {
            sessions.keep_answer(&self.0.session, grant);
        }
        state.witness_answer(&question, request, &answer.edits)
    }

    /// The person's answer on the sheet to the question about `label`, waited for while the
    /// session goes: why not, where it ended or the person dismissed it.
    async fn answer_to(
        &self,
        mut answer: tokio::sync::oneshot::Receiver<crate::app_actions::AppActionAnswer>,
        label: &str,
    ) -> Result<crate::app_actions::AppActionAnswer, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
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

impl ActionHost for AgentConnection {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        self.0.host.query(&self.0.caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        // The session's group, whatever the client says: history lists the step under the
        // session, and taking its work back names it.
        let mut request = request;
        let mut context = request.context.take().unwrap_or_default();
        context.group = self.0.caller.agent_run();
        request.context = Some(context);
        self.0.host.invoke(&self.0.caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.check(caller).ok()?;
        self.0.host.observe(&self.0.caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.check(caller).ok()?;
        self.0.host.next_change(&self.0.caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        self.0.host.cancel(&self.0.caller, id)
    }

    /// Ask the session's person to confirm `request` on the host's sheet.
    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        let runtime = match super::launchable::delivery_runtime() {
            Ok(runtime) => runtime,
            Err(reason) => return Pending::ready(Err(ActionHostError::Unavailable { reason })),
        };
        let connection = self.clone();
        let request = request.clone();
        let (complete, answer) = pending();
        runtime.spawn(async move {
            // A caller that stops waiting withdraws the question: the asking is dropped, and
            // with it the sheet.
            let asked = tokio::select! {
                asked = connection.ask_person(&request) => Some(asked),
                () = complete.abandoned() => None,
            };
            if let Some(asked) = asked {
                complete.complete(asked);
            }
        });
        answer
    }
}
