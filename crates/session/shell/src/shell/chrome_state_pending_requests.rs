//! Session-local requests the chrome holds until they are answered.
//!
//! [`PendingPrompt`] is a person-facing ask with a reply channel back to the
//! portal or agent door that made it; [`RecordRequest`] is a record a peer
//! asked this session to produce, tracked until it is sent. Both are requests
//! waiting on this session, kept together rather than among the chrome's
//! widget-facing overlays and identity state.

use crate::shell::state::{AppId, Capability, WorkspaceId};

/// What a [`PendingPrompt`] asks the person.
#[derive(Debug)]
pub enum PromptRequest {
    /// An app asks for a capability.
    Capability(Capability),
    /// A client in this login asks to work in a workspace as an agent
    /// (the session's agent door).
    AgentAdmission(crate::shell::agent_admission::AgentAdmission),
    /// A program running in an app asks to act beyond the app's workspace
    /// (`tilo do` in a terminal).
    DesktopControl(crate::shell::desktop_control::DesktopControlAsk),
}

/// One capability request waiting for the user to answer it. Dropping
/// without answering drops the `reply` sender, which the portal side
/// reads as deny — refusal, not a hang.
pub struct PendingPrompt {
    /// Session-local identity; a stale answer must never consume another request.
    pub id: uuid::Uuid,
    /// Who asks: an app id, or an agent door client's description.
    pub app_id: AppId,
    pub request: PromptRequest,
    /// Reply channel to a blocked portal request; `None` for
    /// shell-originated prompts nobody is waiting on.
    pub reply: Option<tokio::sync::oneshot::Sender<crate::portal::PromptOutcome>>,
}

impl PendingPrompt {
    /// The shell asking on its own behalf; nobody is blocked on this.
    pub fn new(app_id: AppId, capability: Capability) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            app_id,
            request: PromptRequest::Capability(capability),
            reply: None,
        }
    }

    pub fn with_reply(
        app_id: AppId,
        capability: Capability,
        reply: tokio::sync::oneshot::Sender<crate::portal::PromptOutcome>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            app_id,
            request: PromptRequest::Capability(capability),
            reply: Some(reply),
        }
    }

    /// An agent door client waiting to be admitted.
    pub fn agent_admission(admission: crate::shell::agent_admission::AgentAdmission) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            app_id: admission.facts.subject(),
            request: PromptRequest::AgentAdmission(admission),
            reply: None,
        }
    }

    /// A program in an app asking to act beyond its workspace; `reply`
    /// carries the person's answer.
    pub fn desktop_control(
        ask: crate::shell::desktop_control::DesktopControlAsk,
        reply: tokio::sync::oneshot::Sender<crate::portal::PromptOutcome>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            app_id: ask.app_label.clone(),
            request: PromptRequest::DesktopControl(ask),
            reply: Some(reply),
        }
    }

    /// The capability asked for, when an app asks for one.
    pub fn capability(&self) -> Option<&Capability> {
        match &self.request {
            PromptRequest::Capability(capability) => Some(capability),
            PromptRequest::AgentAdmission(_) | PromptRequest::DesktopControl(_) => None,
        }
    }

    pub fn answer(&mut self, outcome: crate::portal::PromptOutcome) {
        self.answer_as(outcome, None);
    }

    /// Answer, admitting an agent door client as `member` when the person
    /// named one; other prompts ignore it.
    pub fn answer_as(
        &mut self,
        outcome: crate::portal::PromptOutcome,
        member: Option<crate::shell::agent_admission::MemberChoice>,
    ) {
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(outcome);
        }
        if let PromptRequest::AgentAdmission(admission) = &mut self.request {
            admission.answer(outcome, member);
        }
    }

    /// Whether whoever asked has stopped waiting: the prompt is withdrawn.
    pub fn is_retired(&self) -> bool {
        self.reply
            .as_ref()
            .is_some_and(tokio::sync::oneshot::Sender::is_closed)
            || matches!(&self.request, PromptRequest::AgentAdmission(admission) if admission.is_retired())
    }
}

impl std::fmt::Debug for PendingPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPrompt")
            .field("id", &self.id)
            .field("app_id", &self.app_id)
            .field("request", &self.request)
            .field("awaiting_portal_reply", &self.reply.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordRequest {
    pub(crate) request_id: u64,
    pub(crate) record_id: String,
    pub(crate) kind: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) room: crate::shell::state::RoomId,
    pub(crate) actor: crate::shell::state::Actor,
}
