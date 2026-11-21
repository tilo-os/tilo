//! Chat's waiting questions, told to the people they wait for.
//!
//! A question a message asks one person is state of the message: it waits
//! on every device of theirs that reads the conversation. Here the host
//! reads it too, so the person it waits for hears of it with no device on.
//! A question that waits joins their log as a thing that reaches them,
//! with one button for each answer it offers; one that was answered or
//! taken back waits there no more.
//!
//! The host reads only the part of Chat's messages a member just wrote to,
//! and every part once when it starts. A question is told under an id of
//! its own, so reading a part again tells nothing twice.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, json};
use tilo_actions::remote::person_telling::{
    About, BY_ANSWERED, Kind, Offered, OfferedTarget, Tell, Teller as ToldBy, Told,
};
use tilo_resource::Id;

use crate::workspaces::{ToTell, Workspaces};

/// The app that keeps the conversations.
const CHAT: &str = "so.tilo.chat";

/// What one question of Chat's means for the log of the person it asks.
pub(super) enum QuestionUpdate {
    /// It waits: told under its id, once.
    Waits(Box<ToTell>),
    /// Its person answered it, on this device of theirs.
    Answered { id: String, device: String },
    /// Its asker took it back.
    TakenBack { id: String },
}

/// The id the question of `message` in `workspace` is told under.
fn told_id(workspace: &str, message: &Id) -> String {
    format!("question:{workspace}:{message}")
}

/// The person a question asks, by their root.
fn asked(question: &chat::question::Question) -> Option<[u8; 32]> {
    hex::decode(question.person.person_id()?)
        .ok()?
        .try_into()
        .ok()
}

/// What the questions in `part`, a part of `workspace`'s Chat, mean for the
/// logs of the people they ask, oldest first.
pub(super) fn questions(
    workspace: &str,
    part: &chat::parts::ChatMessages,
) -> Vec<([u8; 32], QuestionUpdate)> {
    let mut found = Vec::new();
    for (id, message) in part.messages.iter() {
        let Some(question) = &message.question else {
            continue;
        };
        let Some(person) = asked(question) else {
            continue;
        };
        let told = told_id(workspace, id);
        let update = match question.state() {
            chat::question::QuestionState::Waiting => {
                if message.text.trim().is_empty() {
                    continue;
                }
                QuestionUpdate::Waits(Box::new(telling(workspace, (id, message), question, told)))
            }
            chat::question::QuestionState::Withdrawn(_) => QuestionUpdate::TakenBack { id: told },
            chat::question::QuestionState::Answered(answered)
            | chat::question::QuestionState::Settled(answered, _) => QuestionUpdate::Answered {
                id: told,
                device: answered.device.0.clone(),
            },
        };
        found.push((person, update));
    }
    found
}

/// The waiting `question` of `message` as its person is told it: the
/// message's words, where it is, and a button for each answer it offers,
/// which invokes Chat's own answering verb as the person. No question asks
/// for a secret: Chat refuses one, and a secret is typed into the system's
/// protected field on the person's device instead.
fn telling(
    workspace: &str,
    (id, message): (&Id, &chat::chat::Message),
    question: &chat::question::Question,
    told: String,
) -> ToTell {
    let answering = collab_types::document::chain_resource_id(&chat::parts::chain());
    let actions = question
        .answers
        .iter()
        .enumerate()
        .map(|(place, answer)| {
            let mut arguments = Map::new();
            arguments.insert("message".into(), json!(id.as_str()));
            arguments.insert("answer".into(), json!(answer.id));
            Offered {
                id: answer.id.clone(),
                label: answer.label.clone(),
                main: place == 0,
                target: OfferedTarget {
                    app: CHAT.to_owned(),
                    record: String::new(),
                    resource: answering.clone(),
                    action: chat::parts::ChatMessages::ANSWER_QUESTION.id().to_owned(),
                },
                arguments,
            }
        })
        .collect();
    ToTell {
        id: Some(told),
        kind: Kind::Question,
        tell: Tell {
            words: message.text.clone(),
            about: Some(at_message(workspace, id)),
            actions,
            ..Tell::default()
        },
        from: Some(ToldBy {
            agent: message.author.agent_id().unwrap_or_default().to_owned(),
            name: message.author_name.clone().unwrap_or_default(),
        }),
        workspace: Some(workspace.to_owned()),
        words_stay: false,
        spender: None,
    }
}

/// Where the message `id` of `workspace`'s conversations is, as a thing
/// told about it says where it leads: Chat's own record of the workspace,
/// whose parts keep its messages, at the address Chat opens the message at.
pub(super) fn at_message(workspace: &str, id: &Id) -> About {
    About {
        workspace: workspace.to_owned(),
        app: CHAT.to_owned(),
        record: String::new(),
        resource: tilo_resource::items::item_resource_id::<chat::Chat>("messages", id),
    }
}

/// Whether `route` is a part of Chat's messages.
fn is_part(route: &str) -> bool {
    collab_types::document::DocumentId::parse(route)
        .is_some_and(|part| chat::parts::part_index(&part).is_some())
}

/// Follows the workspaces' conversations for questions that wait for a
/// person, and keeps the logs of what people were told in step with them.
pub struct Feeder {
    workspaces: Workspaces,
}

impl Feeder {
    #[must_use]
    pub const fn new(workspaces: Workspaces) -> Self {
        Self { workspaces }
    }

    /// Read every workspace's conversations once, then each part a member
    /// writes to, until the host stops.
    pub async fn run(self) {
        let mut writes = self.workspaces.document_writes();
        self.look_everywhere().await;
        loop {
            let mut due: BTreeSet<(String, String)> = BTreeSet::new();
            match writes.recv().await {
                Ok(write) => {
                    if write.app == CHAT && is_part(&write.record) {
                        due.insert((write.workspace, write.record));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    self.look_everywhere().await;
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
            // A burst of writes to one part is read once.
            while let Ok(write) = writes.try_recv() {
                if write.app == CHAT && is_part(&write.record) {
                    due.insert((write.workspace, write.record));
                }
            }
            for (workspace, route) in due {
                if let Err(error) = self.look_part(&workspace, &route).await {
                    tracing::debug!(%workspace, %error, "a part of a chat was not read for its questions");
                }
            }
        }
    }

    async fn look_everywhere(&self) {
        for summary in self.workspaces.list().await {
            if let Err(error) = self.look(&summary.workspace).await {
                tracing::debug!(workspace = %summary.workspace, %error, "a chat was not read for its questions");
            }
        }
    }

    /// Read every part of `workspace`'s conversations for its questions.
    ///
    /// # Errors
    /// The part could not be read.
    pub async fn look(&self, workspace: &str) -> Result<(), String> {
        let routes = self
            .workspaces
            .document_routes(workspace)
            .await
            .map_err(|error| error.to_string())?;
        for (app, route) in &routes {
            if app == CHAT && is_part(route) {
                self.look_part(workspace, route).await?;
            }
        }
        Ok(())
    }

    /// Read the part `route` of `workspace`'s conversations: tell each
    /// person the questions that wait for them there, and let go of those
    /// that were answered or taken back.
    ///
    /// # Errors
    /// The part could not be read.
    pub async fn look_part(&self, workspace: &str, route: &str) -> Result<(), String> {
        let Some(bytes) = self
            .workspaces
            .resource_document(workspace, CHAT, route)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        // The part is read and let go of before anything is told: what
        // follows waits on a person's own model and their log.
        let (found, mentions) = {
            let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            let part = crate::workspace_bots::opened::<chat::parts::ChatMessages>(&mut doc)?;
            let now_ms = i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX);
            (
                questions(workspace, &part),
                super::mentions::named(workspace, &part, now_ms),
            )
        };
        self.tell_mentions(workspace, mentions).await;
        // What waits in each person's log, read once a person.
        let mut waiting: BTreeMap<[u8; 32], BTreeSet<String>> = BTreeMap::new();
        for (person, update) in found {
            let waits = |waiting: &mut BTreeMap<[u8; 32], BTreeSet<String>>, id: &str| {
                waiting
                    .entry(person)
                    .or_insert_with(|| {
                        self.workspaces
                            .told_after(&person, 0)
                            .told
                            .into_iter()
                            .filter(Told::waits)
                            .map(|told| told.id)
                            .collect()
                    })
                    .contains(id)
            };
            let kept = match update {
                QuestionUpdate::Waits(telling) => {
                    self.workspaces.tell(&person, *telling).await.map(drop)
                }
                QuestionUpdate::Answered { id, device } if waits(&mut waiting, &id) => self
                    .workspaces
                    .acknowledge_told(&person, &id, &device, BY_ANSWERED)
                    .map(drop),
                QuestionUpdate::TakenBack { id } if waits(&mut waiting, &id) => {
                    self.workspaces.withdraw_told(&person, &id).map(drop)
                }
                QuestionUpdate::Answered { .. } | QuestionUpdate::TakenBack { .. } => Ok(()),
            };
            if let Err(error) = kept {
                tracing::debug!(%workspace, %error, "a question was not kept in its person's log");
            }
        }
        Ok(())
    }

    /// Tell each person `mentions` names that they were mentioned, under the
    /// name the message keeps for its author, else the one the workspace
    /// shows for them.
    async fn tell_mentions(&self, workspace: &str, mentions: Vec<super::mentions::Named>) {
        let id = tilo_server_protocol::WorkspaceId::parse(workspace);
        for named in mentions {
            let mut author = named
                .author_name
                .clone()
                .filter(|name| !name.trim().is_empty());
            if author.is_none()
                && let (Some(id), Some(root)) = (id, super::mentions::root_of(&named.author))
            {
                author = self.workspaces.name_of_member(id, root).await;
            }
            let author = author.unwrap_or_else(|| "Someone".to_owned());
            let person = named.person;
            let telling = super::mentions::telling(workspace, named, &author);
            if let Err(error) = self.workspaces.tell(&person, telling).await {
                tracing::debug!(%workspace, %error, "a mention was not told to its person");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use chat::chat::Message;
    use chat::question::{Answered, Choice, Question, Withdrawn};
    use tilo_actions::MemberId;
    use tilo_resource::Atom;

    use super::*;

    const ADA: [u8; 32] = [8; 32];
    const WORKSPACE: &str = "6f1f0f7e-3c1d-4a54-9b6e-2d1c5a7e9f10";

    fn question(kind: &str, answers: &[(&str, &str)]) -> Question {
        Question {
            kind: Atom(kind.to_owned()),
            person: MemberId::person(hex::encode(ADA)),
            answers: answers
                .iter()
                .map(|(id, label)| Choice {
                    id: (*id).to_owned(),
                    label: (*label).to_owned(),
                })
                .collect(),
            detail: Atom(String::new()),
            answered: None,
            outcome: None,
            withdrawn: None,
        }
    }

    fn message(text: &str, question: Option<Question>) -> Message {
        let assistant = MemberId::agent("assistant");
        let conversation =
            chat::chat::Chat::direct_id([MemberId::person(hex::encode(ADA)), assistant.clone()]);
        let mut message = Message::new(conversation, assistant, 1_763_366_400, text.to_owned());
        message.author_name = Some("Assistant".to_owned());
        message.question = question;
        message
    }

    /// A question that waits is told the person it asks, with a button
    /// for each answer that invokes Chat's own answering verb; a question
    /// answered or taken back waits no more; a plain message is nobody's
    /// question.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one part with a question in each state, read once"
    )]
    fn a_parts_questions_are_read_as_they_stand() {
        let decks = [("march", "The March one"), ("june", "The June one")];
        let mut answered = question(chat::question::CHOICE, &decks);
        answered.answered = Some(Answered {
            answer: Atom("march".to_owned()),
            device: Atom("Ada's phone".to_owned()),
            at: 1,
            detail: Atom(String::new()),
        });
        let mut taken_back = question(chat::question::CHOICE, &decks);
        taken_back.withdrawn = Some(Withdrawn {
            at: 2,
            why: String::new(),
        });
        let mut part = chat::parts::ChatMessages::default();
        for (id, message) in [
            ("m0", message("Nine mails sorted since seven.", None)),
            (
                "m1",
                message(
                    "Two versions of the deck exist. Which one?",
                    Some(question(chat::question::CHOICE, &decks)),
                ),
            ),
            ("m2", message("Which deck?", Some(answered))),
            ("m3", message("Which deck?", Some(taken_back))),
        ] {
            part.messages.push(Id::from_raw(id.to_owned()), message);
        }

        let found = questions(WORKSPACE, &part);
        assert_eq!(found.len(), 3, "the plain message asks nobody");
        assert!(found.iter().all(|(person, _)| *person == ADA));

        let QuestionUpdate::Waits(waits) = &found[0].1 else {
            panic!("the first question waits");
        };
        assert_eq!(
            waits.id.as_deref(),
            Some("question:6f1f0f7e-3c1d-4a54-9b6e-2d1c5a7e9f10:m1")
        );
        assert_eq!(waits.kind, Kind::Question);
        assert_eq!(
            waits.tell.words,
            "Two versions of the deck exist. Which one?"
        );
        assert!(!waits.words_stay);
        assert_eq!(
            waits.from,
            Some(ToldBy {
                agent: "assistant".to_owned(),
                name: "Assistant".to_owned(),
            })
        );
        let about = waits.tell.about.clone().unwrap();
        assert_eq!(
            (
                about.workspace.as_str(),
                about.app.as_str(),
                about.record.as_str()
            ),
            (WORKSPACE, CHAT, "")
        );
        assert!(
            about.resource.ends_with("/messages/m1"),
            "{}",
            about.resource
        );
        let buttons: Vec<(&str, &str, bool)> = waits
            .tell
            .actions
            .iter()
            .map(|action| (action.id.as_str(), action.label.as_str(), action.main))
            .collect();
        assert_eq!(
            buttons,
            [
                ("march", "The March one", true),
                ("june", "The June one", false)
            ]
        );
        let first = &waits.tell.actions[0];
        assert_eq!(
            (first.target.app.as_str(), first.target.action.as_str()),
            (CHAT, "answer-question")
        );
        assert!(
            first.target.resource.starts_with("chain:"),
            "{}",
            first.target.resource
        );
        assert_eq!(
            serde_json::Value::Object(first.arguments.clone()),
            json!({"message": "m1", "answer": "march"})
        );

        assert!(matches!(
            &found[1].1,
            QuestionUpdate::Answered { id, device }
                if id.ends_with(":m2") && device == "Ada's phone"
        ));
        assert!(matches!(&found[2].1, QuestionUpdate::TakenBack { id } if id.ends_with(":m3")));
    }
}
