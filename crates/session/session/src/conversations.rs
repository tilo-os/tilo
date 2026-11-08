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

    /// Whether `self` and `other` give different turns to deliver: the same
    /// workspaces, the same agents, and the same runs in each. The policy they
    /// were decided against is not part of it — a grant changing is announced
    /// elsewhere, and delivery is about which turns go, not what they may do.
    fn differs_from(&self, other: &Self) -> bool {
        self.workspaces.len() != other.workspaces.len()
            || self
                .workspaces
                .iter()
                .zip(&other.workspaces)
                .any(|(was, now)| {
                    was.workspace != now.workspace
                        || was.turns != now.turns
                        || was.agents != now.agents
                })
    }
}

/// The turns, as the broker's run channel reads them, and what their agents
/// spent.
pub(crate) struct Runs {
    held: Mutex<Held>,
    /// What the runs' agents spent since the last pass, in millionths of the
    /// currency, by workspace and agent: the pass charges it to their day.
    spent: Mutex<BTreeMap<(String, String), u64>>,
    /// The total each run last reported, by workspace, task and epoch, so a
    /// report charges only what it added.
    reported: Mutex<BTreeMap<(String, String, u64), u64>>,
    /// Wakes the session's loop when a run reported what it spent, so the day
    /// it is charged to is the day the agent then works in.
    wake: Option<tilo_service::Wake>,
}

impl Runs {
    /// The turns, woken through `wake` when a run reports what it spent.
    fn new(wake: tilo_service::Wake) -> Self {
        Self {
            held: Mutex::default(),
            spent: Mutex::default(),
            reported: Mutex::default(),
            wake: Some(wake),
        }
    }

    /// What the runs' agents spent since the last pass, by workspace and
    /// agent, in millionths of the currency, ready to be charged to their day.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        let Ok(mut spent) = self.spent.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *spent)
            .into_iter()
            .map(|((workspace, agent), cost)| (workspace, agent, cost))
            .collect()
    }

    /// Keep only the reports of the turns `held` names: a run that ended is
    /// charged for what it spent, and its total is no longer followed.
    fn prune_reported(&self, held: &Held) {
        let Ok(mut reported) = self.reported.lock() else {
            return;
        };
        reported.retain(|(workspace, issue, epoch), _| {
            held.workspaces
                .iter()
                .find(|facts| &facts.workspace == workspace)
                .is_some_and(|facts| {
                    facts
                        .turns
                        .iter()
                        .any(|entry| &entry.run.issue == issue && entry.run.epoch == *epoch)
                })
        });
    }

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

    /// What a run spent, as its agent reported it: charged to the agent's day
    /// by the pass, and only for what it added to the total it reported
    /// before.
    fn reported(
        &self,
        workspace: &str,
        issue: &str,
        epoch: u64,
        update: &runtime_collab::TaskRunUpdate,
    ) {
        let runtime_collab::TaskRunUpdate::Spent { cost_micros } = update else {
            return;
        };
        let agent = self.held.lock().ok().and_then(|held| {
            held.workspaces
                .iter()
                .find(|facts| facts.workspace == workspace)
                .and_then(|facts| {
                    facts
                        .turns
                        .iter()
                        .find(|entry| entry.run.issue == issue && entry.run.epoch == epoch)
                })
                .map(|entry| entry.run.agent.clone())
        });
        let Some(agent) = agent else {
            // The turn is over: what it spent was charged when the pass read
            // the report before it.
            return;
        };
        let added = {
            let Ok(mut reported) = self.reported.lock() else {
                return;
            };
            let key = (workspace.to_owned(), issue.to_owned(), epoch);
            let before = reported.insert(key, *cost_micros).unwrap_or_default();
            cost_micros.saturating_sub(before)
        };
        if added == 0 {
            return;
        }
        if let Ok(mut spent) = self.spent.lock() {
            let cost = spent.entry((workspace.to_owned(), agent)).or_default();
            *cost = cost.saturating_add(added);
        }
        if let Some(wake) = &self.wake {
            wake();
        }
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
        let waking = Arc::clone(&wake);
        if let Err(error) = scope.bind(move |_| waking()) {
            tracing::warn!(%error, "the conversations are not woken");
        }
        Self {
            runs: Arc::new(Runs::new(wake)),
            owners: RecordOwners::new(root),
            wake: scope,
            open: RefCell::default(),
        }
    }

    /// The turns, as the broker reads them.
    pub(crate) fn runs(&self) -> Arc<Runs> {
        Arc::clone(&self.runs)
    }

    /// What the runs' agents spent since the last pass, by workspace and
    /// agent, in millionths of the currency, for their day to be charged.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        self.runs.take_spent()
    }

    /// Chat's record of `workspace`, as this session holds it open: the
    /// conversations a person asked in and their messages, read through the
    /// same open record the pass reads turns from — never a second writer on
    /// the store. `None` where the workspace holds no Chat record here.
    #[must_use]
    pub fn chat(&self, workspace: &str) -> Option<chat::Chat> {
        let open = self.open.borrow();
        let opened = open.get(workspace)?;
        Some(opened.chat.as_ref()?.get().as_ref().clone())
    }

    /// What the canvas asks of the session, made as `person`'s in
    /// `workspace`: their ask conversation with the agent the workspace
    /// keeps, the agent making it (shell.md §8.1, "Asking from the canvas").
    /// The conversation is a Chat channel the agent takes part in, so the
    /// person's message in it is a turn the broker delivers to that agent,
    /// and both are made with Chat's own actions on its own record: the
    /// session invents no record, no conversation and no message.
    ///
    /// Returns the resource the conversation is published as
    /// (`chat.workspace/channels/<id>`), so the canvas places the very
    /// channel Chat shows, drawn by Chat's own view — never a second chat
    /// (shell.md §8.1, "The view"). `None` when there is no workspace in
    /// front to ask in, no agent the workspace keeps, or Chat refused.
    pub(crate) fn ask(
        &self,
        person: &str,
        workspace: &str,
        policy: &WorkspacePolicy,
        agent: &str,
        text: &str,
    ) -> Option<String> {
        let Some((chat, part)) = self.opened_with(
            &WorkspaceFacts {
                workspace: workspace.to_owned(),
                policy: policy.clone(),
            },
            person,
            true,
        ) else {
            tracing::warn!(%workspace, "the ask opened no Chat record");
            return None;
        };
        // The conversation is the person's and the agent's, by their keys: a
        // participant key is what a conversation is with
        // (`Conversation::participants`), and the agent's answers the
        // person's messages in it.
        let participants = vec![
            chat::record::Participant::person(person).key(),
            chat::record::Participant::agent(agent).key(),
        ];
        // The id a kept conversation with these participants has, derived the
        // same on every device: a channel id, because the conversation is one
        // — it is made once and named in the place's address — never a direct
        // conversation's id, which names its participants and is nothing the
        // record can hold.
        let id = chat::record::Chat::conversation_channel_id(
            participants
                .iter()
                .filter_map(|key| chat::record::Participant::parse(key)),
        );
        let channel = id.as_str().to_owned();
        // The conversation is made once, with Chat's own `create-channel-with-id`:
        // a second ask meets the one that is there, as a step run again does.
        let name = tilo_resource::NonBlank::parse(&crate::i18n::t("session-ask-conversation"))?;
        let made = chat.invoke(&chat::Chat::CREATE_CHANNEL_WITH_ID.with(
            chat::record::chat::CreateChannelWithId {
                id: channel,
                name,
                category: None,
                kind: None,
                topic: None,
                participants: Some(participants),
            },
        ));
        if let Err(error) = made {
            tracing::warn!(%error, %workspace, "the ask conversation was not made");
            return None;
        }
        // The person's question, as the post Chat's composer makes: the
        // message is theirs, so the agent's reply is the one its id derives
        // (`reply_id`), and the turn is the message's.
        let posted = part.invoke(&chat::parts::ChatPart::POST.with(
            chat::messages::chat_part::Post {
                message: chat::messages::NewMessage {
                    channel: id.clone(),
                    text: text.to_owned(),
                    parent: None,
                    body: None,
                    attachments: Vec::new(),
                    mentions: None,
                    written: None,
                },
            },
        ));
        if let Err(error) = posted {
            tracing::warn!(%error, %workspace, "the person's question was not posted");
            return None;
        }
        // The conversation as Chat publishes a channel: its item resource, so a
        // place of it is the channel Chat's own window shows, drawn by Chat's
        // `conversation` view (`chat.workspace/channels/<id>`), never the
        // record as a whole.
        Some(tilo_resource::items::item_resource_id::<chat::Chat>(
            "channels", &id,
        ))
    }

    /// Read each workspace's conversations as `person`, and hand the turns
    /// they give to the run channel.
    ///
    /// A pass that read different turns than the one before announces them
    /// ([`tilo_identity_host::policy_change::announce`]): the broker re-reads
    /// what it delivers when that changes, not on a clock, so a new turn — the
    /// person typing an ask into a conversation Chat had no message in — is
    /// only handed to its agent because this says it exists. Nothing is
    /// announced while the turns are the ones already read, so a quiet desk
    /// costs the broker no re-read.
    pub(crate) fn feed(&self, person: &str, workspaces: Vec<WorkspaceFacts>) {
        let read: Vec<Facts> = workspaces
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
        let held = Held { workspaces: read };
        self.runs.prune_reported(&held);
        let changed = match self.runs.held.lock() {
            Ok(mut current) => {
                let changed = current.differs_from(&held);
                *current = held;
                changed
            }
            Err(error) => {
                // A poisoned run source keeps the turns it already held: this
                // pass's read was not stored, so announcing it would have the
                // broker re-read turns that are not there. The session's log
                // says the delivery stopped, rather than the ask failing
                // silently.
                tracing::warn!(%error, "the turns' run source is poisoned; this pass delivers nothing");
                false
            }
        };
        // Announced after the read is in place: the broker that wakes re-reads
        // what this just stored, never the pass before it.
        if changed {
            tilo_identity_host::policy_change::announce();
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
        self.opened_with(workspace, person, false)
    }

    /// [`Self::opened`], and `create`: whether a workspace that holds no Chat
    /// yet is given one, with the genesis every device derives the same, and
    /// the first part of its messages. What asking does — a person who never
    /// opened Chat still has a conversation to ask in (shell.md §8.1, "Asking
    /// from the canvas") — and reading never does: the pass creates no record
    /// of an app no one used.
    fn opened_with(
        &self,
        workspace: &WorkspaceFacts,
        person: &str,
        create: bool,
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
        if create && opened.chat.is_none() {
            // The workspace holds no Chat, and the person asked in it: the
            // record is made here, with the genesis every device derives the
            // same, so a second device's first ask meets the one this made.
            opened.chat = WorkspaceRecord::<chat::Chat>::open_primary(
                &opened.records,
                CHAT_OWNER,
                CHAT_APP,
                Some(LivePermissions::fixed(permissions.clone())),
                Some(self.wake.handle()),
            );
        }
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
            opened.chat = WorkspaceRecord::<chat::Chat>::open_primary(
                &opened.records,
                CHAT_OWNER,
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
            .max();
        // The chain holds no part until one is made: the first ask makes part
        // 0, whose id every device derives the same, and reads it from then on.
        let head = match (head, create) {
            (Some(head), _) => head,
            (None, true) => 0,
            (None, false) => return None,
        };
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

/// The person a message's author names, when it names one: the participant key
/// (`person:<root>`, what a conversation's fields keep) or the bare id a record
/// write attributes its change to ([`ActionCx::author`](tilo_resource::action)).
/// Both spell one person — the second is what Chat writes a message's author
/// as — so a message a person posted summons its conversation's agent however
/// it was written, and the run answers for the person it names.
fn person_of(author: &str) -> Option<String> {
    match chat::record::Participant::parse(author) {
        Some(chat::record::Participant::Person(root)) if !root.is_empty() => return Some(root),
        // A participant key naming an agent, or an empty person.
        Some(chat::record::Participant::Person(_) | chat::record::Participant::Agent(_)) => {
            return None;
        }
        None => {}
    }
    // A bare id: not a participant key at all, and no agent's either.
    (!author.is_empty()
        && !author.contains(':')
        && !author.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX))
    .then(|| author.to_owned())
}

/// The turns `chat`'s conversations give, read from `part`'s messages, and the
/// agents the conversations take in.
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
            // answer to that message carries. An agent's reply that is still
            // working (its work is not finished) leaves the turn going, so a
            // runner that records "taken" before it acts is not read as
            // having answered.
            let summons = messages
                .iter()
                .copied()
                .filter(|(message_id, message)| {
                    !message.text.trim().is_empty()
                        && person_of(&message.author.0).is_some()
                        && part
                            .messages
                            .get(&chat::record::reply_id(message_id, &key))
                            // A reply without work is a plain answer; one
                            // that keeps work is going until its work ends.
                            .is_none_or(|reply| {
                                reply.work.as_ref().is_some_and(|work| {
                                    let streamed = work
                                        .stream
                                        .as_ref()
                                        .is_some_and(|stream| stream.finish.is_some());
                                    !streamed
                                        && work
                                            .state
                                            .as_ref()
                                            .is_none_or(|transition| !transition.state.finished())
                                })
                            })
                })
                .max_by_key(|(message_id, message)| (message.ts, message_id.as_str()));
            let Some((message_id, message)) = summons else {
                continue;
            };
            let Some(starter) = person_of(&message.author.0) else {
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

    /// A message Chat writes names its author by the bare id the record write
    /// gives, not by the `person:<root>` key its conversation keeps: both spell
    /// one person, so a message a person posts in Chat summons its agent as
    /// surely as one written under the key.
    #[test]
    fn a_message_written_by_a_bare_person_id_is_a_turn_too() {
        let bare = "22503e79b0b80274014ec6333d3561ca89cb3209937b901c4bc2e4bedccc2f85";
        let chat = workspace();
        let head = part(vec![("m1", message(bare, "what is on my task list?", 10))]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert_eq!(entry.run.agent, AGENT);
        assert_eq!(entry.run.starter, bare, "the run acts for that person");
        assert_eq!(entry.run.instruction, "what is on my task list?");
        assert!(
            entry.run.active,
            "the person's message waits for its answer"
        );
        assert_eq!(
            person_of("agent-task:22222222-2222-4222-8222-222222222222"),
            None,
            "an agent's message is no person's turn"
        );
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
    fn a_runs_spend_is_charged_to_its_agent_for_what_each_report_added() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let (entries, agents) = turns(&chat, &head, ME);
        let issue = entries[0].run.issue.clone();
        let epoch = entries[0].run.epoch;
        let agent = entries[0].run.agent.clone();
        let runs = Runs::new(std::sync::Arc::new(|| {}));
        *runs.held.lock().unwrap() = Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents,
                policy: None,
                turns: entries,
            }],
        };
        let spent = |cost_micros| runtime_collab::TaskRunUpdate::Spent { cost_micros };

        runs.reported("w1", &issue, epoch, &spent(420_000));
        assert_eq!(
            runs.take_spent(),
            vec![("w1".to_owned(), agent.clone(), 420_000)]
        );
        assert_eq!(runs.take_spent(), Vec::new(), "a charge is taken once");

        runs.reported("w1", &issue, epoch, &spent(420_000));
        assert_eq!(
            runs.take_spent(),
            Vec::new(),
            "a repeated total adds nothing"
        );

        runs.reported("w1", &issue, epoch, &spent(900_000));
        assert_eq!(
            runs.take_spent(),
            vec![("w1".to_owned(), agent, 480_000)],
            "the run's total, not the last call's, is what its agent spent"
        );

        runs.reported("w2", &issue, epoch, &spent(100));
        assert_eq!(
            runs.take_spent(),
            Vec::new(),
            "a turn this session does not hold is not charged"
        );
    }

    #[test]
    fn the_run_channel_knows_only_the_workspaces_it_read() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let (entries, agents) = turns(&chat, &head, ME);
        let runs = Runs::new(std::sync::Arc::new(|| {}));
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

    /// A pass that read different turns than the one before is what wakes the
    /// broker to deliver a new one: the person typing an ask into a
    /// conversation Chat held no message in is only handed to its agent
    /// because [`Held::differs_from`] says the turns changed.
    #[test]
    fn a_new_turn_is_a_change_and_the_same_turns_are_not() {
        let chat = workspace();
        let asked = part(vec![("m1", message(ME, "what is on my task list?", 10))]);
        let (entries, agents) = turns(&chat, &asked, ME);
        let facts = |turns: Vec<Entry>| Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents: agents.clone(),
                policy: None,
                turns,
            }],
        };
        let nothing = Held::default();
        let waiting = facts(entries.clone());
        assert!(
            nothing.differs_from(&waiting),
            "a turn that was not there is a change to deliver"
        );
        assert!(
            !waiting.differs_from(&facts(entries)),
            "the same turns delivered again are no change"
        );
        assert!(
            facts(Vec::new()).differs_from(&waiting),
            "a turn that ended is a change too: its run stops"
        );
    }
}
