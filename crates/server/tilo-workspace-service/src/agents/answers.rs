//! The answer to a question an agent on this server asked wakes that agent,
//! once.
//!
//! An agent that needs its person asks in the conversation
//! ([`chat::question`]) and its turn ends: nothing is done and nothing is paid
//! for while the question waits, an hour or a week, across every restart of
//! this server. When the person answers, on any device of theirs, the server
//! starts one turn of the asker through Chat's `ask-agent`, under a cause
//! whose key is the question's own (`question:<message id>`). One key is one
//! message and one turn however often the server looks and however often it
//! restarts, so an answer wakes its asker once and only once. The turn is
//! told what it asked and what its person answered, with the conversation
//! before it like any turn.
//!
//! Nobody is woken for a question that still waits, that its asker took
//! back, or whose asker already said what came of the answer. Where the
//! person's own words follow their answer in the conversation, those words
//! are what asks the agent, as ever, and no second turn is started beside
//! them. A question about an action the server keeps
//! ([`super::asks`]) is carried out by the server first, and its asker is then
//! told how it ended, under the same key.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde_json::{Map, json};
use tilo_actions::remote::Caller;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::Agent;

use super::{CHAT, ServerAgents, WorkspaceActionProvider, invoke_named};

/// The kind of the cause a wake carries: its person answered a question the
/// agent asked them.
pub(super) const ANSWER: &str = "answer";
/// Who writes a wake into the conversation: no agent of the workspace's,
/// acting for the person who answered.
const WAKER: &str = "question";
/// The longest label a wake's cause carries, in bytes.
const MOST_LABEL_BYTES: usize = 100;

/// The wakes Chat refused, by workspace and key: a conversation no agent
/// takes part in takes none, and asking again at every look would change
/// nothing.
static REFUSED: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// One turn an answer owes the agent that asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Wake {
    /// The key of its cause: the question's own, so it is written once.
    pub(super) key: String,
    /// What kind of cause it is: [`ANSWER`], or news of what the server did
    /// with the answer.
    kind: &'static str,
    /// The person who answered, by their person id: the turn is for them.
    person: String,
    /// The conversation the question stands in.
    conversation_id: String,
    /// What a person calls it: the question's first words.
    label: String,
    /// What the agent is told.
    text: String,
}

/// The key of the cause that wakes the asker of the question `message`
/// asks.
pub(super) fn key_of(message: &str) -> String {
    format!("question:{message}")
}

/// `words` as a cause's label: their first line, short.
fn label_of(words: &str) -> String {
    let mut label = String::new();
    for character in words.lines().next().unwrap_or_default().chars() {
        if character.is_control() {
            continue;
        }
        if label.len().saturating_add(character.len_utf8()) > MOST_LABEL_BYTES {
            break;
        }
        label.push(character);
    }
    let label = label.trim();
    if label.is_empty() {
        "A question".to_owned()
    } else {
        label.to_owned()
    }
}

/// Whether the server carries the answer to `question` out itself: a question
/// about an action that names what a server kept. Its asker is told once that
/// is done, not when it is answered.
fn carried_by_the_server(question: &chat::question::Question) -> bool {
    question.kind.0 == chat::question::ACTION
        && question
            .kept()
            .is_some_and(|detail| detail.get("ask").is_some() || detail.get("use").is_some())
}

/// The turns the answered questions in `part` owe `agents`: one for each
/// question one of them asked that its person answered, that nobody settled
/// or took back, and whose answer their own words do not follow.
pub(super) fn answered_in(
    part: &chat::parts::ChatMessages,
    agents: &BTreeMap<String, Agent>,
) -> Vec<Wake> {
    part.messages
        .iter()
        .filter_map(|(id, message)| {
            let question = message.question.as_ref()?;
            let chat::question::QuestionState::Answered(given) = question.state() else {
                return None;
            };
            agents.get(message.author.agent_id()?)?;
            let person = question.person.person_id()?;
            if carried_by_the_server(question) {
                return None;
            }
            // Their own words after the answer ask the agent already.
            let (asked, since) = (&question.person, given.at);
            let followed = part.messages.iter().any(|(_, other)| {
                other.channel == message.channel
                    && other.author == *asked
                    && other.cause.is_none()
                    && other.ts >= since
            });
            if followed {
                return None;
            }
            let said = question
                .label(&given.answer.0)
                .unwrap_or(given.answer.0.as_str());
            Some(Wake {
                key: key_of(id.as_str()),
                kind: ANSWER,
                person: person.to_owned(),
                conversation_id: message.channel.to_string(),
                label: label_of(&message.text),
                text: format!(
                    "Your person answered a question you asked them.\nYou asked: {}\nThey \
                     answered: {said}\nGo on from where you asked.",
                    message.text.trim()
                ),
            })
        })
        .collect()
}

/// The keys of the wakes `part` holds already: every cause that names a
/// question.
pub(super) fn woken_in(part: &chat::parts::ChatMessages) -> impl Iterator<Item = String> + '_ {
    part.messages.iter().filter_map(|(_, message)| {
        let key = &message.cause.as_ref()?.key.0;
        key.starts_with("question:").then(|| key.clone())
    })
}

/// What the asker of a question the server carried out is told: how its
/// person answered `asked` (the server's words for the action), and what came
/// of it.
pub(super) fn carried_out(
    (message, conversation_id): (&str, &str),
    person: &str,
    asked: &str,
    (answer, came): (&str, &str),
) -> Wake {
    Wake {
        key: key_of(message),
        kind: chat::cause::NEWS,
        person: person.to_owned(),
        conversation_id: conversation_id.to_owned(),
        label: label_of(asked),
        text: format!(
            "Your person answered the question about an action you wanted to take.\nThe \
             action: {asked}\nThey answered: {answer}\n{came}\nDo not take it again for this \
             answer. Go on from where you asked."
        ),
    }
}

impl ServerAgents {
    /// Start the turns `owed` owes in `workspace`, each on a task of its
    /// own: a look does not wait for them.
    pub(super) fn wake_answered(&self, workspace: &str, owed: Vec<Wake>) {
        for wake in owed {
            if REFUSED
                .lock_recover()
                .contains(&(workspace.to_owned(), wake.key.clone()))
            {
                continue;
            }
            let (this, workspace) = (self.clone(), workspace.to_owned());
            tokio::spawn(async move {
                if let Err(error) = this.wake(&workspace, &wake).await {
                    tracing::warn!(%workspace, key = %wake.key, %error, "an answer did not wake its asker");
                    REFUSED.lock_recover().insert((workspace, wake.key));
                }
            });
        }
    }

    /// Write `wake` into its conversation: the message Chat's `ask-agent`
    /// keeps under its key, for the person who answered. Written again under
    /// the same key it changes nothing.
    ///
    /// # Errors
    /// The person is admitted in `workspace` on no device, or Chat refused
    /// the ask: no agent takes part in that conversation.
    pub(super) async fn wake(&self, workspace: &str, wake: &Wake) -> Result<(), String> {
        let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
        let person: [u8; 32] = hex::decode(&wake.person)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("the question names no person")?;
        let device = self
            .workspaces
            .person_device(workspace_id, person)
            .await
            .ok_or("the person who answered is admitted here on no device")?;
        // The person's own answer wakes the agent: an automation of theirs, with their authority.
        let waker = Caller::authenticated(person, device)
            .in_workspace(workspace)
            .running_shortcut(WAKER);
        let mut arguments = Map::new();
        arguments.insert("text".into(), json!(wake.text));
        arguments.insert("channel".into(), json!(wake.conversation_id));
        arguments.insert(
            "cause".into(),
            json!({"kind": wake.kind, "label": wake.label, "key": wake.key}),
        );
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: workspace.to_owned(),
            document: String::new(),
        };
        invoke_named(
            &WorkspaceActionProvider::new(self.actions.clone(), workspace_id),
            &waker,
            &route,
            &collab_types::document::chain_resource_id(&chat::parts::chain()),
            "ask-agent",
            arguments,
        )
        .await
        .map(drop)
    }
}
