//! The runs a person's conversations give this session's agents.
//!
//! An agent works inside a turn: a person's message in a conversation the
//! agent takes part in ([`tilo_action_policy::turn`]). A run is that turn, as
//! the broker's run channel delivers it to the agent: the person's newest
//! message in the conversation that the agent has not answered yet.
//!
//! Nothing here keeps a record of its own: the conversation's own record says
//! what is asked and what was answered. A message summons its conversation's
//! agents, and an agent's answer is the reply with the id its summons and its
//! id derive ([`chat::record::reply_id`]), so a turn is over when its reply
//! exists and a later message is a turn of its own. What the run's ops carry
//! is the turn; what the receiving device looked up about its cause (who wrote
//! it, whether the agent takes part, whether the turn ended) is read from the
//! same record here.
//!
//! Conversations are items of Chat's workspace record and their messages are
//! its parts ([`chat::parts`]), so this reads Chat's record through the same
//! store every app of the workspace opens, as the workspace's settings tabs
//! read the workspace's agents.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_action_broker::services::{NativeTaskActionRequirement, TrackerRun, TrackerRuns};
use tilo_action_policy::WorkspacePolicy;
use tilo_action_policy::turn::{Looked, Turn};
use tilo_app::framework::WorkspaceRecord;
use tilo_clock::wall;
use tilo_resource::{Id, Resource as _};
use ui::permissions::{LivePermissions, PermissionContext, UserId};
use ui::record::workspace::WorkspaceRecords;
use ui::record::{RecordId, RecordOwners};

/// The app that keeps the conversations: its own record of a workspace.
const CHAT_OWNER: &str = "so.tilo.chat";
/// How the run source names Chat where a record is opened.
const CHAT_APP: (&str, &str) = ("chat", "Chat");

/// One workspace's conversations as the pass read them, and the runs they
/// give.
#[derive(Clone, Default)]
struct Facts {
    workspace: String,
    /// The agents the workspace admits now, by their bare id.
    agents: Vec<String>,
    /// Its grants: a run's op is admitted by these.
    policy: Option<WorkspacePolicy>,
    /// The turns going in it: one per conversation and agent.
    turns: Vec<Entry>,
}

/// A turn going, as this device read it: the run the broker delivers, and what
/// the same read says about its cause.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    run: TrackerRun,
    looked: Looked,
}

/// What the pass read: each workspace's going turns.
#[derive(Default)]
struct Held {
    workspaces: Vec<Facts>,
}

impl Held {
    fn workspace(&self, workspace: &str) -> Result<&Facts, String> {
        self.workspaces
            .iter()
            .find(|facts| facts.workspace == workspace)
            .ok_or_else(|| format!("this session holds no workspace {workspace}"))
    }
}

/// The turns, as the broker's run channel reads them.
#[derive(Default)]
pub(crate) struct Runs {
    held: Mutex<Held>,
}

impl Runs {
    fn workspace(&self, workspace: &str) -> Result<Facts, String> {
        self.held
            .lock()
            .map_err(|_| "this session's conversations are unavailable")?
            .workspace(workspace)
            .cloned()
    }
}

impl TrackerRuns for Runs {
    /// Only a workspace this session holds: its conversations are its own.
    fn requires_workspace(&self, workspace: &str) -> bool {
        self.held.lock().is_ok_and(|held| {
            held.workspaces
                .iter()
                .any(|facts| facts.workspace == workspace)
        })
    }

    /// An agent works only inside its turns, never on a workspace grant
    /// alone: what it may do is its person's grants inside the turn.
    fn confines_agents_to_tasks(&self, workspace: &str) -> bool {
        self.requires_workspace(workspace)
    }

    fn runs(&self, workspace: &str) -> Result<Vec<TrackerRun>, String> {
        Ok(self
            .workspace(workspace)?
            .turns
            .iter()
            .map(|entry| entry.run.clone())
            .collect())
    }

    /// Whether the agent still takes part in a conversation of this
    /// workspace: that is where it works, and an agent a conversation no
    /// longer takes in has no turn to answer.
    fn check_agent(&self, workspace: &str, run: &TrackerRun) -> Result<(), String> {
        let facts = self.workspace(workspace)?;
        if facts.agents.contains(&run.agent) {
            Ok(())
        } else {
            Err(format!(
                "the agent {} takes part in no conversation here",
                run.agent
            ))
        }
    }

    fn authorize_action(
        &self,
        workspace: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        let facts = self.workspace(workspace)?;
        let entry = facts
            .turns
            .iter()
            .find(|entry| entry.run.issue == run.issue && entry.run.agent == run.agent)
            .ok_or("the turn this op answers is no longer going")?;
        let policy = facts
            .policy
            .as_ref()
            .ok_or("this workspace's grants are not at hand")?;
        requirement.check_tracker_policy(policy, workspace, &entry.run, Some(&entry.looked), wall())
    }
}

/// One workspace as the pass hands it over: the grants its turns are decided
/// against.
pub(crate) struct WorkspaceFacts {
    pub(crate) workspace: String,
    pub(crate) policy: WorkspacePolicy,
}

/// The conversations this session's agents answer in, read from Chat's record
/// of each workspace it holds.
pub(crate) struct Conversations {
    runs: Arc<Runs>,
    /// Where this device keeps its apps' records.
    owners: RecordOwners,
    /// Wakes the session's loop when a change to a record lands.
    wake: ui::WakeScope,
    /// Each workspace's records, opened here and followed change by change.
    open: RefCell<BTreeMap<String, Opened>>,
}

/// A workspace's records as they are open here, and what was read of them.
struct Opened {
    /// The grants they were opened with: a workspace whose grants changed has
    /// them opened again, so its decisions follow.
    policy: WorkspacePolicy,
    records: WorkspaceRecords,
    chat: Option<WorkspaceRecord<chat::Chat>>,
    /// The head of the messages' chain, and the part it holds here.
    part: Option<(u64, WorkspaceRecord<chat::parts::ChatPart>)>,
}

impl Conversations {
    /// The conversations over the store at `root`, waking the session's loop
    /// through `wake` when a change lands.
    pub(crate) fn new(root: PathBuf, wake: tilo_service::Wake) -> Self {
        let mut scope = ui::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "the conversations are not woken");
        }
        Self {
            runs: Arc::default(),
            owners: RecordOwners::new(root),
            wake: scope,
            open: RefCell::default(),
        }
    }

    /// The turns, as the broker reads them.
    pub(crate) fn runs(&self) -> Arc<Runs> {
        Arc::clone(&self.runs)
    }

    /// Read each workspace's conversations as `person`, and hand the turns
    /// they give to the run channel.
    pub(crate) fn feed(&self, person: &str, workspaces: Vec<WorkspaceFacts>) {
        let read = workspaces
            .into_iter()
            .map(|facts| {
                let (turns, agents) = self.turns(&facts, person);
                Facts {
                    workspace: facts.workspace,
                    agents,
                    policy: Some(facts.policy),
                    turns,
                }
            })
            .collect();
        if let Ok(mut held) = self.runs.held.lock() {
            *held = Held { workspaces: read };
        }
    }

    /// The turns `workspace`'s conversations give, read at the revisions its
    /// records are at now, and the agents those conversations take in.
    fn turns(&self, workspace: &WorkspaceFacts, person: &str) -> (Vec<Entry>, Vec<String>) {
        match self.opened(workspace, person) {
            Some((chat, part)) => {
                let chat_record = chat.get();
                let part_record = part.get();
                turns(&chat_record, &part_record, person)
            }
            None => (Vec::new(), Vec::new()),
        }
    }

    /// The workspace's chat record and the head of its messages, opened here
    /// where they are not, and the turns read from them at the revisions they
    /// are at.
    fn opened(
        &self,
        workspace: &WorkspaceFacts,
        person: &str,
    ) -> Option<(
        WorkspaceRecord<chat::Chat>,
        WorkspaceRecord<chat::parts::ChatPart>,
    )> {
        let mut open = self.open.borrow_mut();
        let id = workspace.workspace.clone();
        let stale = open
            .get(&id)
            .is_some_and(|opened| opened.policy != workspace.policy);
        if stale {
            open.remove(&id);
        }
        let opened = open.entry(id).or_insert_with(|| Opened {
            policy: workspace.policy.clone(),
            records: WorkspaceRecords::in_workspace(
                self.owners.clone(),
                workspace.workspace.clone(),
                UserId(person.to_owned()),
            ),
            chat: None,
            part: None,
        });
        let permissions = PermissionContext::from_standing(
            &workspace.workspace,
            tilo_action_policy::Standing::person(workspace.policy.clone(), person.to_owned()),
        )
        .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        let chat_id = RecordId::package_primary();
        if opened.chat.is_none() {
            // A workspace whose person never opened Chat holds no record: the
            // reader creates none.
            let held = opened
                .records
                .holds_record(CHAT_OWNER, chat_id.as_str(), chat::Chat::KIND)
                .unwrap_or(false);
            if !held {
                return None;
            }
            opened.chat = WorkspaceRecord::<chat::Chat>::open_at(
                &opened.records,
                CHAT_OWNER,
                chat_id.as_str(),
                CHAT_APP,
                Some(LivePermissions::fixed(permissions.clone())),
                Some(self.wake.handle()),
            );
        }
        let chat = opened.chat.clone()?;
        let head = opened
            .records
            .store_of(CHAT_OWNER)
            .ok()?
            .held_parts(&chat::parts::chain())
            .into_iter()
            .max()?;
        if opened.part.as_ref().map(|(held, _)| *held) != Some(head) {
            let part = WorkspaceRecord::<chat::parts::ChatPart>::open_at(
                &opened.records,
                CHAT_OWNER,
                chat::parts::part(head).as_str(),
                CHAT_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
            opened.part = part.map(|part| (head, part));
        }
        let part = opened.part.as_ref().map(|(_, part)| part.clone())?;
        Some((chat, part))
    }
}

/// The turns `chat`'s conversations give, read from `part`'s messages, and the
/// agents the conversations take in.
///
/// A conversation the pass has no message for is none: a direct conversation
/// exists only as the id its messages carry ([`chat::Chat::direct_id`]).
fn turns(
    chat: &chat::Chat,
    part: &chat::parts::ChatPart,
    person: &str,
) -> (Vec<Entry>, Vec<String>) {
    let mut conversations: Vec<Id> = Vec::new();
    for (_, message) in part.messages.iter() {
        if !conversations.contains(&message.channel) {
            conversations.push(message.channel.clone());
        }
    }
    let mut turns = Vec::new();
    let mut agents = Vec::new();
    for id in &conversations {
        // An archived conversation takes no message, and one the record does
        // not list is a direct conversation the record derives from its id.
        if chat
            .channels
            .get(id)
            .is_some_and(|conversation| conversation.archived)
        {
            continue;
        }
        let messages: Vec<(&Id, &chat::record::Message)> = part
            .messages
            .iter()
            .filter(|(_, message)| message.channel == *id)
            .collect();
        for participant in chat.conversation_participants(id) {
            let key = participant.key();
            let chat::record::Participant::Agent(agent) = participant else {
                continue;
            };
            if !agents.contains(&agent) {
                agents.push(agent.clone());
            }
            // The person's newest message the agent has not answered: the
            // reply its id and the agent's derive, which only the agent's
            // answer to that message carries.
            let summons = messages
                .iter()
                .copied()
                .filter(|(message_id, message)| {
                    !message.text.trim().is_empty()
                        && matches!(
                            chat::record::Participant::parse(&message.author.0),
                            Some(chat::record::Participant::Person(who)) if !who.is_empty()
                        )
                        && part
                            .messages
                            .get(&chat::record::reply_id(message_id, &key))
                            .is_none()
                })
                .max_by_key(|(message_id, message)| (message.ts, message_id.as_str()));
            let Some((message_id, message)) = summons else {
                continue;
            };
            let Some(chat::record::Participant::Person(starter)) =
                chat::record::Participant::parse(&message.author.0)
            else {
                continue;
            };
            // The conversation's address in the workspace, as the desk and the
            // apps name it, and the message's own beneath it.
            let address = format!("{}/channels/{id}", chat::Chat::KIND);
            turns.push(Entry {
                run: TrackerRun {
                    issue: format!("{address}/messages/{message_id}"),
                    title: chat.display_names(id, Some(person)),
                    instruction: message.text.clone(),
                    agent: agent.clone(),
                    epoch: 1,
                    starter,
                    active: true,
                    finished_by_agent: false,
                    paused_through: None,
                    budget: None,
                    subject: None,
                    model: None,
                    directions: Vec::new(),
                    question: None,
                    turn: Some(Turn::new(address, message_id.to_string())),
                    delegation: None,
                },
                looked: Looked::by(message.author.0.clone()),
            });
        }
    }
    (turns, agents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Atom;

    /// The agent the tests' conversations take in.
    const AGENT: &str = "tide";
    /// The id its words are written under.
    const AGENT_ACTOR: &str = "agent:tide";
    /// The person sitting here.
    const ME: &str = "person:me";
    /// The conversation the two of them have.
    const CONVERSATION: &str = "c_tide";

    /// A workspace holding one conversation between `ME` and `AGENT`.
    fn workspace() -> chat::Chat {
        let mut chat = chat::Chat::default();
        chat.channels.push(
            Id::from_raw(CONVERSATION),
            chat::record::Conversation {
                name: "Tide".into(),
                participants: vec![Atom(ME.into()), Atom(AGENT_ACTOR.into())],
                ..chat::record::Conversation::default()
            },
        );
        chat
    }

    /// A message `author` wrote in `CONVERSATION` at `ts`.
    fn message(author: &str, text: &str, ts: i64) -> chat::record::Message {
        chat::record::Message {
            channel: Id::from_raw(CONVERSATION),
            author: Atom(author.into()),
            ts,
            text: text.into(),
            ..chat::record::Message::default()
        }
    }

    /// The head part of the messages, holding `messages`.
    fn part(messages: Vec<(&str, chat::record::Message)>) -> chat::parts::ChatPart {
        let mut part = chat::parts::ChatPart::default();
        for (id, message) in messages {
            part.messages.push(Id::from_raw(id), message);
        }
        part
    }

    /// The turns `chat`'s conversations give for the messages of `head`.
    fn read(chat: &chat::Chat, head: &chat::parts::ChatPart) -> Vec<Entry> {
        turns(chat, head, ME).0
    }

    #[test]
    fn an_unanswered_message_is_a_turn_of_the_conversations_agent() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert_eq!(entry.run.agent, AGENT);
        assert_eq!(entry.run.starter, "me");
        assert_eq!(entry.run.instruction, "add the revenue curve");
        assert_eq!(entry.run.title, "Tide");
        assert_eq!(
            entry.run.turn,
            Some(Turn::new("chat.workspace/channels/c_tide", "m1"))
        );
        assert_eq!(entry.looked, Looked::by(ME));
        assert!(entry.run.active);
    }

    #[test]
    fn the_agents_reply_ends_its_turn() {
        let chat = workspace();
        let reply = chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR);
        let mut head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        head.messages.push(reply, message(AGENT_ACTOR, "done", 20));
        assert_eq!(read(&chat, &head), Vec::new());
    }

    #[test]
    fn the_newest_unanswered_message_is_the_turn() {
        let chat = workspace();
        let head = part(vec![
            ("m1", message(ME, "first", 10)),
            ("m2", message(ME, "second", 20)),
        ]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert_eq!(entry.run.instruction, "second");
    }

    #[test]
    fn an_agents_own_words_are_no_summons() {
        let chat = workspace();
        let head = part(vec![("m1", message(AGENT_ACTOR, "on it", 10))]);
        assert_eq!(read(&chat, &head), Vec::new());
    }

    #[test]
    fn a_channel_of_the_whole_workspace_summons_no_agent() {
        let mut chat = workspace();
        chat.channels.push(
            Id::from_raw("c_general"),
            chat::record::Conversation {
                name: "general".into(),
                ..chat::record::Conversation::default()
            },
        );
        let head = part(vec![(
            "m1",
            chat::record::Message {
                channel: Id::from_raw("c_general"),
                ..message(ME, "hello all", 10)
            },
        )]);
        assert_eq!(read(&chat, &head), Vec::new());
    }

    #[test]
    fn the_run_channel_knows_only_the_workspaces_it_read() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let (entries, agents) = turns(&chat, &head, ME);
        let runs = Runs::default();
        *runs.held.lock().unwrap() = Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents,
                policy: None,
                turns: entries,
            }],
        };
        assert!(runs.requires_workspace("w1"));
        assert!(!runs.requires_workspace("w2"));
        assert_eq!(runs.runs("w1").unwrap().len(), 1);
        assert!(runs.runs("w2").is_err());
        let mut run = runs.runs("w1").unwrap().remove(0);
        assert!(runs.check_agent("w1", &run).is_ok());
        run.agent = "other".into();
        assert!(runs.check_agent("w1", &run).is_err());
    }
}
