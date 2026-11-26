//! Chat's action questions and the turn that asked them. General actions, vault uses,
//! and account connections recover the caller from the same persisted turn fields.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::policy::MemberId;
use tilo_actions::remote::Caller;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Resource as _;
use tilo_workspace_agent::Agent;

use super::{CHAT, ServerAgents, WorkspaceActionProvider, WorkspaceId, invoke_named};

/// Where a question was asked. These fields are part of the stored ask format.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct AskedIn {
    conversation: String,
    cause: String,
    session: String,
}

impl AskedIn {
    pub(super) fn of(caller: &Caller) -> Self {
        let turn = caller.turn();
        Self {
            conversation: turn
                .map(|turn| turn.conversation.clone())
                .unwrap_or_default(),
            cause: turn.map(|turn| turn.message.clone()).unwrap_or_default(),
            session: caller
                .agent_session()
                .map(|session| session.id)
                .unwrap_or_default(),
        }
    }
}

pub(super) fn asked_in(caller: &Caller) -> Value {
    serde_json::to_value(AskedIn::of(caller)).unwrap_or(Value::Null)
}

/// The identities a recovered ask uses to act on its turn and report the outcome after it.
pub(super) struct AskCallers {
    pub person: [u8; 32],
    pub turn: Caller,
    pub outside: Caller,
}

impl ServerAgents {
    pub(super) async fn ask_callers(
        &self,
        workspace_id: WorkspaceId,
        agent: &Agent,
        person: &str,
        asked_in: &Value,
    ) -> Option<AskCallers> {
        let mut root = [0; 32];
        hex::decode_to_slice(person, &mut root).ok()?;
        self.workspaces.person_device(workspace_id, root).await?;
        let asked_in: AskedIn = serde_json::from_value(asked_in.clone()).ok()?;
        let outside = self
            .agent_caller(workspace_id, agent, root)
            .await
            .ok()?
            .in_agent_session(asked_in.session);
        let turn = outside
            .clone()
            .on_roster_turn(tilo_action_policy::turn::Turn::new(
                asked_in.conversation,
                asked_in.cause,
            ));
        Some(AskCallers {
            person: root,
            turn,
            outside,
        })
    }

    /// Every part of the chat's messages `workspace` holds, by its document.
    pub(super) async fn chat_parts(
        &self,
        workspace: &str,
    ) -> Vec<(String, chat::parts::ChatMessages)> {
        let routes = self
            .workspaces
            .document_routes(workspace)
            .await
            .unwrap_or_default();
        let mut parts = Vec::new();
        for (app, document) in routes {
            if app != CHAT
                || collab_types::document::DocumentId::parse(&document)
                    .is_none_or(|part| chat::parts::part_index(&part).is_none())
            {
                continue;
            }
            if let Ok(Some(part)) = self
                .read::<chat::parts::ChatMessages>(workspace, CHAT, &document)
                .await
            {
                parts.push((document, part));
            }
        }
        parts
    }
}

/// Post Chat's native action question as the turn's caller. The caller supplies the exact
/// detail it kept for comparison when an answer arrives.
pub(super) async fn post(
    (service, caller): (&WorkspaceActionProvider, &Caller),
    workspace: &str,
    conversation: &str,
    text: &str,
    detail: Value,
    takes_always: bool,
) -> Result<(), String> {
    let mut answers = vec![json!({"id": chat::question::ALLOW})];
    if takes_always {
        answers.push(json!({"id": chat::question::ALWAYS}));
    }
    answers.push(json!({"id": chat::question::DENY}));
    let mut arguments = Map::new();
    arguments.insert("channel".into(), json!(conversation));
    arguments.insert("text".into(), json!(text));
    arguments.insert("kind".into(), json!(chat::question::ACTION));
    arguments.insert("answers".into(), Value::Array(answers));
    arguments.insert("detail".into(), detail);
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    invoke_named(
        service,
        caller,
        &route,
        &collab_types::document::chain_resource_id(&chat::parts::chain()),
        "ask",
        arguments,
    )
    .await
    .map(drop)
}

pub(super) struct Posted<'a> {
    pub document: &'a str,
    pub id: &'a tilo_resource::Id,
    pub message: &'a chat::chat::Message,
    pub question: &'a chat::question::Question,
    pub answered_as_asked: bool,
}

/// Find the published question matching the server's saved detail and both identities.
/// A positive answer can be used only if the sole matching message has never been edited.
pub(super) fn posted<'a>(
    parts: &'a [(String, chat::parts::ChatMessages)],
    agent: &str,
    person: &str,
    detail: &Value,
) -> Option<Posted<'a>> {
    let agent = MemberId::agent(agent);
    let person = MemberId::person(person);
    let mut matching = parts
        .iter()
        .flat_map(|(document, part)| {
            part.messages
                .iter()
                .map(move |(id, message)| (document, id, message))
        })
        .filter_map(|(document, id, message)| {
            let question = message.question.as_ref()?;
            (message.author == agent
                && question.person == person
                && question.kind.0 == chat::question::ACTION
                && question.kept().as_ref() == Some(detail))
            .then_some((document, id, message, question))
        });
    let (document, id, message, question) = matching.next()?;
    // Pending questions are common during a look. Only an answer needs the uniqueness check.
    let answered_as_asked = matches!(question.state(), chat::question::QuestionState::Answered(_))
        && !message.edited
        && message.history.is_empty()
        && matching.next().is_none();
    Some(Posted {
        document,
        id,
        message,
        question,
        answered_as_asked,
    })
}

/// Say on the question `message` in the part `document` what came of its
/// answer.
pub(super) async fn settle(
    (service, asker): (&WorkspaceActionProvider, &Caller),
    workspace: &str,
    (document, message): (&str, &str),
    outcome: &str,
    told: Option<String>,
) -> Result<(), String> {
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: document.to_owned(),
    };
    let mut arguments = Map::new();
    arguments.insert("message".into(), json!(message));
    arguments.insert("outcome".into(), json!(outcome));
    if let Some(told) = told {
        arguments.insert("detail".into(), json!(told));
    }
    let resource = format!("{}/messages/{message}", chat::parts::ChatMessages::KIND);
    // The answer was read in the conversation as the workspace's store holds it: the settle is
    // made on that, not on what the replica it writes through held before its person answered.
    service.landed(route.clone());
    invoke_named(
        service,
        asker,
        &route,
        &resource,
        "settle-question",
        arguments,
    )
    .await
    .map(drop)
}
