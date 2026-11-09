//! The runs a person's conversations give this session's agents.
//! An agent works inside a turn: a person's message in a conversation the agent takes part in
//! ([`tilo_action_policy::turn`]). A run is that turn, as the broker's run channel delivers it to
//! the agent: the person's newest message in the conversation that the agent has not answered yet.
//! Nothing here keeps a record of its own: the conversation's own record says what is asked and
//! what was answered. A message summons its conversation's agents, and an agent's answer is the
//! reply with the id its summons and its id derive ([`chat::record::reply_id`]), so a turn is over
//! when its reply exists and a later message is a turn of its own. What the run's ops carry is the
//! turn; what the receiving device looked up about its cause (who wrote it, whether the agent takes
//! part, whether the turn ended) is read from the same record here.
//! Conversations are items of Chat's workspace record and their messages are its parts
//! ([`chat::parts`]), so this reads Chat's record through the same store every app of the workspace
//! opens, as the workspace's settings tabs read the workspace's agents.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
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
/// Chat's verb that records a turn's work on the reply to its message: the one Chat's window
/// publishes on each message, run here on the same record from the same declaration.
const RECORD_WORK: &str = "record-work";
/// The chat action that asks an agent again for a turn it could not answer.
const RETRY: &str = "retry";

/// One workspace's conversations as the pass read them, and the runs they give.
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

/// A turn going, as this device read it: the run the broker delivers, and what the same read says
/// about its cause.
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

/// The turns, as the broker's run channel reads them, and what their agents spent.
pub(crate) struct Runs {
    held: Mutex<Held>,
    /// What the runs' agents spent since the last pass, in millionths of the currency, by workspace
    /// and agent: the pass charges it to their day.
    spent: Mutex<BTreeMap<(String, String), u64>>,
    /// The total each run last reported, by workspace, task and epoch, so a report charges only
    /// what it added.
    reported: Mutex<BTreeMap<(String, String, u64), u64>>,
    /// Wakes the session's loop when a run reported what it spent, so the day it is charged to is
    /// the day the agent then works in.
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

    /// What the runs' agents spent since the last pass, by workspace and agent, in millionths of
    /// the currency, ready to be charged to their day.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        let Ok(mut spent) = self.spent.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *spent)
            .into_iter()
            .map(|((workspace, agent), cost)| (workspace, agent, cost))
            .collect()
    }

    /// Keep only the reports of the turns `held` names: a run that ended is charged for what it
    /// spent, and its total is no longer followed.
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

    /// An agent works only inside its turns, never on a workspace grant alone: what it may do is
    /// its person's grants inside the turn.
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

    /// Whether the agent still takes part in a conversation of this workspace: that is where it
    /// works, and an agent a conversation no longer takes in has no turn to answer.
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

    /// What a run spent, as its agent reported it: charged to the agent's day by the pass, and only
    /// for what it added to the total it reported before.
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
            // The turn is over: what it spent was charged when the pass read the report before it.
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

/// One workspace as the pass hands it over: the grants its turns are decided against.
pub(crate) struct WorkspaceFacts {
    pub(crate) workspace: String,
    pub(crate) policy: WorkspacePolicy,
}

/// The conversations this session's agents answer in, read from Chat's record of each workspace it
/// holds.
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
    /// The grants they were opened with: a workspace whose grants changed has them opened again, so
    /// its decisions follow.
    policy: WorkspacePolicy,
    records: WorkspaceRecords,
    chat: Option<WorkspaceRecord<chat::Chat>>,
    /// The head of the messages' chain, and the part it holds here.
    part: Option<(u64, WorkspaceRecord<chat::parts::ChatPart>)>,
}

impl Conversations {
    /// The conversations over the store at `root` , waking the session's loop through `wake` when a
    /// change lands.
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

    /// What the runs' agents spent since the last pass, by workspace and agent, in millionths of
    /// the currency, for their day to be charged.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        self.runs.take_spent()
    }

    /// Chat's record of `workspace` , as this session holds it open: the conversations a person
    /// asked in and their messages, read through the same open record the pass reads turns from —
    /// never a second writer on the store. `None` where the workspace holds no Chat record here.
    #[must_use]
    pub fn chat(&self, workspace: &str) -> Option<chat::Chat> {
        let open = self.open.borrow();
        let opened = open.get(workspace)?;
        Some(opened.chat.as_ref()?.get().as_ref().clone())
    }

    /// The person seated here: whose changes what the service records are theirs, and the author
    /// a message or a record of work is written by when the caller names nobody else.
    #[must_use]
    pub fn person(&self) -> String {
        let open = self.open.borrow();
        open.values()
            .next()
            .map(|opened| opened.records.author().0.clone())
            .unwrap_or_default()
    }

    /// The messages people wrote in `channel`, as the session holds them: what a test reads to
    /// prove a retry asked again rather than posting the question a second time.
    #[must_use]
    pub fn person_messages(&self, channel: &str) -> Vec<String> {
        let open = self.open.borrow();
        for opened in open.values() {
            let Some((_, part)) = &opened.part else {
                continue;
            };
            let part = part.get();
            let held: Vec<String> = part
                .messages
                .iter()
                .filter(|(_, message)| {
                    message.channel.as_str() == channel && person_of(&message.author.0).is_some()
                })
                .map(|(id, _)| id.as_str().to_owned())
                .collect();
            if !held.is_empty() {
                return held;
            }
        }
        Vec::new()
    }

    /// The page of the conversation `channel` (a channel id, after
    /// `chat.workspace/channels/`), as Chat's own `#[view(default)]` draws it, read from the
    /// workspace this session holds open that has it: the very channel Chat's window shows and
    /// the canvas places when a person asks there (shell.md §8.1, "The view"). Its messages are
    /// the head part's, projected exactly as the window projects them.
    ///
    /// `None` where no open workspace holds the channel, or the session has not opened one yet.
    #[must_use]
    pub fn channel_reading(&self, channel: &str) -> Option<chat::views::Channel> {
        let open = self.open.borrow();
        for opened in open.values() {
            let Some(chat_record) = &opened.chat else {
                continue;
            };
            let chat = chat_record.get();
            if !chat.channels.iter().any(|(id, _)| id.as_str() == channel) {
                continue;
            }
            let Some((index, part)) = &opened.part else {
                continue;
            };
            let held = chat::parts::ChatPart::clone(part.get().as_ref());
            let messages = chat::messages::Messages::new(vec![(*index, std::rc::Rc::new(held))]);
            let rows = chat.message_rows(&messages, channel);
            let me = opened.records.author().0.clone();
            let names = self.channel_names(opened, &me);
            return Some(chat::views::Channel::read(
                &chat, &rows, channel, &me, &names,
            ));
        }
        None
    }

    /// The names a conversation's page completes its people with, beyond what Chat's own record
    /// says: the name this device shows the seated person by — the very name the top bar shows
    /// them — and the workspace's agents, by their id.
    ///
    /// A session's Chat record may never have been told who reads it (an ask creates the
    /// conversation and posts, and Chat's `introduce` is another app's act), nor which agents the
    /// workspace keeps: those records are here, so the page is read with them and nobody is
    /// shown a routing id.
    fn channel_names(&self, opened: &Opened, me: &str) -> chat::views::Names {
        let mut names = chat::views::Names {
            me: crate::directory::desk_person_name(
                ui::local_identity::load().map(|identity| identity.display_name),
                me,
            ),
            agents: BTreeMap::new(),
        };
        let record = self.workspace_agents(opened);
        if let Some(record) = record {
            let state = record.get();
            for agent in state.active() {
                let name = agent.name.trim();
                if !name.is_empty() {
                    names.agents.insert(agent.id.0.clone(), name.to_owned());
                }
            }
        }
        names
    }

    /// The workspace's agents record `opened` holds, opened here where it is not: the names a
    /// conversation's agents go by. `None` where the workspace keeps none — reading an app's
    /// record creates none, as [`Self::opened_with`] reads Chat's.
    fn workspace_agents(
        &self,
        opened: &Opened,
    ) -> Option<WorkspaceRecord<tilo_app::tilo_workspace_agent::WorkspaceAgents>> {
        use tilo_app::tilo_workspace_agent::{KIND as AGENTS_KIND, OWNER as AGENTS_OWNER};
        let id = collab_types::workspace_record_id(AGENTS_KIND);
        let held = opened
            .records
            .holds_record(AGENTS_OWNER, &id, AGENTS_KIND)
            .unwrap_or(false);
        if !held {
            return None;
        }
        let standing = tilo_action_policy::Standing::person(
            opened.policy.clone(),
            opened.records.author().0.clone(),
        );
        let permissions = PermissionContext::from_standing(&opened.policy.workspace, standing)
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        WorkspaceRecord::<tilo_app::tilo_workspace_agent::WorkspaceAgents>::open(
            &opened.records,
            AGENTS_OWNER,
            CHAT_APP,
            Some(LivePermissions::fixed(permissions)),
            None,
        )
    }

    /// Post `text` in `channel`, as the person seated here, through Chat's own `post`: a message
    /// in the workspace whose open record holds the channel, written to the head part as Chat's
    /// composer writes it. This is the verb a place's composer runs, so the page Chat's window
    /// shows and the page a place draws post to one record.
    ///
    /// # Errors
    /// No open workspace holds the channel, or Chat refused the post.
    pub fn post(&self, channel: &str, text: &str) -> Result<(), String> {
        self.post_span(channel, text, None, None)
    }

    /// Post `text` in `channel` as the person seated here, answering the message `parent` and —
    /// when `span` names a run of it — quoting that run. The words of the run are read from the
    /// answered message here, where it is held, so the reply's quote carries exactly what the
    /// person chose; its offsets are kept, so pressing the quote opens on them.
    ///
    /// Chat's own `post` writes the message, so a reply from a place and one from Chat's window
    /// land in one record, and a span reply is an ordinary reply that carries a quote.
    ///
    /// # Errors
    /// No open workspace holds the channel, the run is not one of the answered message, or Chat
    /// refused the post.
    pub fn post_span(
        &self,
        channel: &str,
        text: &str,
        parent: Option<&str>,
        span: Option<(usize, usize)>,
    ) -> Result<(), String> {
        let open = self.open.borrow();
        for opened in open.values() {
            let Some(chat_record) = &opened.chat else {
                continue;
            };
            let chat = chat_record.get();
            if !chat.channels.iter().any(|(id, _)| id.as_str() == channel) {
                continue;
            }
            let Some((_, part)) = &opened.part else {
                continue;
            };
            let channel = Id::from_raw(channel);
            let quoted = match (parent, span) {
                (Some(parent), Some((start, end))) => {
                    let answered = part
                        .get()
                        .messages
                        .get(&Id::from_raw(parent))
                        .map(|message| message.text.clone())
                        .ok_or_else(|| {
                            format!("the message {parent} being answered is not here")
                        })?;
                    let words = answered
                        .get(start..end)
                        .filter(|words| !words.trim().is_empty())
                        .ok_or_else(|| {
                            format!("{start}..{end} is no run of the message {parent}")
                        })?;
                    Some(chat::record::Quote {
                        text: words.to_owned(),
                        start: u64::try_from(start)
                            .map_err(|_| "an offset too large".to_owned())?,
                        end: u64::try_from(end).map_err(|_| "an offset too large".to_owned())?,
                    })
                }
                _ => None,
            };
            return part
                .invoke(
                    &chat::parts::ChatPart::POST.with(chat::messages::chat_part::Post {
                        message: chat::messages::NewMessage {
                            channel,
                            text: text.to_owned(),
                            parent: parent.filter(|parent| !parent.is_empty()).map(Id::from_raw),
                            quoted,
                            body: None,
                            attachments: Vec::new(),
                            mentions: None,
                            written: None,
                        },
                    }),
                )
                .map_err(|error| error.to_string());
        }
        Err(format!("no open workspace holds the channel {channel}"))
    }

    /// Ask the agent again for a turn's answer, through Chat's own `retry` on the part that holds
    /// the message: the message `arguments` names moves to its next attempt, whose reply no one
    /// has written, so its turn is delivered again and the answer lands in a reply of its own
    /// (shell.md §8.1). Nothing is posted: the person's words stay the one message they wrote.
    ///
    /// # Errors
    /// No open workspace holds the message in `arguments`, or Chat refused the retry.
    fn retry(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::DoorError> {
        let text = |name: &str| {
            arguments
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        let channel = text("channel");
        let reply = text("reply");
        let summons = text("summons");
        if reply.is_empty() || summons.is_empty() {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "a retry names the reply and the message it answered".to_owned(),
            ));
        }
        let open = self.open.borrow();
        for opened in open.values() {
            let Some((_, part)) = &opened.part else {
                continue;
            };
            if opened.chat.is_none() || part.get().messages.get(&Id::from_raw(summons)).is_none() {
                continue;
            }
            if !opened.chat.as_ref().is_some_and(|chat_record| {
                chat_record
                    .get()
                    .channels
                    .iter()
                    .any(|(id, _)| id.as_str() == channel)
            }) {
                continue;
            }
            return part
                .invoke_as(
                    invoker,
                    &chat::parts::ChatPart::RETRY.with(chat::messages::chat_part::Retry {
                        message: Id::from_raw(summons),
                        reply: Id::from_raw(reply),
                    }),
                )
                .map_err(|error| tilo_actions::DoorError::Failed(error.to_string()));
        }
        Err(tilo_actions::DoorError::Failed(format!(
            "no open workspace holds the message {summons} of channel {channel}"
        )))
    }

    /// Record a turn's work through Chat's own `record-work` on the workspace's open Chat record,
    /// as `invoker` (the agent that answered, or the person here): `arguments` are the verb's,
    /// exactly as Chat's window's item takes them, including the `message` whose work it records —
    /// the summons, named as the item Chat's window publishes it names it
    /// (`chat.workspace.messages:message:<id>`) or by the bare id its item carries. Chat's own verb
    /// derives the reply from the summons and the author, so what it records lands on the very
    /// reply the turn's answer is — the address the agent works from, before the reply exists. The
    /// record is the one Chat's window writes and the pass reads turns from: no second writer, and
    /// no second verb.
    ///
    /// # Errors
    /// No open workspace holds the message `arguments` names, or Chat refused the record.
    fn record_work(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::DoorError> {
        let unknown = || {
            tilo_actions::DoorError::Unknown(tilo_actions::ActionAddress::new(
                resource,
                RECORD_WORK,
            ))
        };
        // The message is the agent's argument, as Chat's window's item takes it; the item's own
        // id names it too, for a caller that names nothing.
        let named = arguments
            .get("message")
            .and_then(serde_json::Value::as_str)
            .and_then(message_of)
            .or_else(|| message_of(resource));
        let Some(message) = named else {
            return Err(unknown());
        };
        let work = arguments.get("work").cloned().ok_or_else(|| {
            tilo_actions::DoorError::InvalidArguments(
                "a record of work says how the work is going".to_owned(),
            )
        })?;
        let work: chat::record::WorkUpdate = serde_json::from_value(work).map_err(|error| {
            tilo_actions::DoorError::InvalidArguments(format!("the work does not read: {error}"))
        })?;
        let say = arguments
            .get("say")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let summons = Id::from_raw(message);
        let invoke =
            chat::parts::ChatPart::RECORD_WORK.with(chat::messages::chat_part::RecordWork {
                message: summons.clone(),
                work,
                say,
            });
        let open = self.open.borrow();
        for opened in open.values() {
            let Some((_, part)) = &opened.part else {
                continue;
            };
            if opened.chat.is_none() || part.get().messages.get(&summons).is_none() {
                continue;
            }
            return part
                .invoke_as(invoker, &invoke)
                .map_err(|error| tilo_actions::DoorError::Failed(error.to_string()));
        }
        Err(unknown())
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
        // The conversation is the person's and the agent's, by their keys: a participant key is
        // what a conversation is with (`Conversation::participants`), and the agent's answers the
        // person's messages in it.
        let participants = vec![
            chat::record::Participant::person(person).key(),
            chat::record::Participant::agent(agent).key(),
        ];
        // The id a kept conversation with these participants has, derived the same on every device:
        // a channel id, because the conversation is one — it is made once and named in the place's
        // address — never a direct conversation's id, which names its participants and is nothing
        // the record can hold.
        let id = chat::record::Chat::conversation_channel_id(
            participants
                .iter()
                .filter_map(|key| chat::record::Participant::parse(key)),
        );
        let channel = id.as_str().to_owned();
        // The conversation is made once, with Chat's own `create-channel-with-id` : a second ask
        // meets the one that is there, as a step run again does.
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
        // The person's question, as the post Chat's composer makes: the message is theirs, so the
        // agent's reply is the one its id derives (`reply_id`), and the turn is the message's.
        let posted = part.invoke(&chat::parts::ChatPart::POST.with(
            chat::messages::chat_part::Post {
                message: chat::messages::NewMessage {
                    channel: id.clone(),
                    text: text.to_owned(),
                    parent: None,
                    quoted: None,
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
        // The conversation as Chat publishes a channel: its item resource, so a place of it is the
        // channel Chat's own window shows, drawn by Chat's `conversation` view
        // (`chat.workspace/channels/<id>`), never the record as a whole.
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

    /// The turns `workspace` 's conversations give, read at the revisions its records are at now,
    /// and the agents those conversations take in.
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

    /// The workspace's chat record and the head of its messages, opened here where they are not,
    /// and the turns read from them at the revisions they are at.
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

    /// [`Self::opened`], and `create` : whether a workspace that holds no Chat yet is given one,
    /// with the genesis every device derives the same, and the first part of its messages. What
    /// asking does — a person who never opened Chat still has a conversation to ask in (shell.md
    /// §8.1, "Asking from the canvas") — and reading never does: the pass creates no record of an
    /// app no one used.
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
            // The workspace holds no Chat, and the person asked in it: the record is made here,
            // with the genesis every device derives the same, so a second device's first ask meets
            // the one this made.
            opened.chat = WorkspaceRecord::<chat::Chat>::open_primary(
                &opened.records,
                CHAT_OWNER,
                CHAT_APP,
                Some(LivePermissions::fixed(permissions.clone())),
                Some(self.wake.handle()),
            );
        }
        if opened.chat.is_none() {
            // A workspace whose person never opened Chat holds no record: the reader creates none.
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

/// Chat's conversations as a session service, so a place that shows one draws Chat's own
/// channel page from its reading (one-app-model.md §2, shell.md §8.1): the very channel Chat's
/// window shows, drawn by Chat's `#[view(default)]`, never a second chat.
///
/// The reading is read from the workspace's open Chat record, through the same record the
/// agent pass reads turns from — never a second writer on the store. It is the real path a
/// person's session uses, not a development fixture: the canvas's ask places
/// `chat.workspace/channels/<id>`, and the shell reads that reading here.
pub(crate) struct ChatViewService {
    conversations: Rc<Conversations>,
    /// The summonses it published last, keyed by each open chat's revision and messages' part:
    /// rebuilt only when a part moved or a workspace opened, so a pass that changed nothing
    /// writes no item again — and reads no part, which would cost the loop every frame.
    summonses: std::cell::RefCell<Option<(SummonsKey, Vec<tilo_actions::ResourceSpec>)>>,
}

/// What the summonses a [session's Chat service](ChatViewService) publishes depend on: each open
/// chat's revision, its messages' head part's revision, and the part's number.
type SummonsKey = (u64, u64, u64);

impl ChatViewService {
    /// The service over `conversations`, the session's one open of the workspaces' chats.
    #[must_use]
    pub(crate) fn new(conversations: Rc<Conversations>) -> Self {
        Self {
            conversations,
            summonses: std::cell::RefCell::new(None),
        }
    }
}

/// One conversation as a place's resource, the way Chat publishes a channel of its record: its
/// item id (`chat.workspace/channels/<id>`), its `page` view (Chat's own `#[view(default)]` over
/// the channel's type), its type, and the composer's `post`. A place made from it shows the
/// channel through that view — never the app's window, which is what a resource naming no view
/// falls back to.
///
/// A workspace channel is named with its `#`, as Chat writes a channel; a conversation kept with
/// participants (the ask) is named plainly, as the window names a conversation, never with a
/// `#` it does not wear.
#[must_use]
pub(crate) fn channel_resource(
    id: &str,
    name: &str,
    is_channel: bool,
) -> tilo_actions::ResourceSpec {
    let label = if is_channel {
        format!("#{name}")
    } else {
        name.to_owned()
    };
    let mut spec = tilo_actions::ResourceSpec::new(
        format!("chat.workspace/channels/{id}"),
        "chat.workspace.channels",
        label,
    )
    .with_view(tilo_actions::ResourceViewSpec::new(
        "page",
        name.to_owned(),
        "open",
    ))
    .with_type(tilo_actions::ResourceType {
        noun: tilo_actions::Noun::new("channel", "channels"),
        app: tilo_actions::Noun::new("Chat", "Chat"),
        root: false,
    });
    spec.app_id = Some("chat".to_owned());
    spec.with_action(retry_action()).with_action(post_action())
}

/// The channel's `retry` action, as the service publishes it on a channel's item: ask the agent
/// again for the turn whose answer failed, on the same message — the very verb Chat's own
/// `retry` runs, reached from the page's failure row and from the REPL alike (shell.md §8.1,
/// "The REPL").
///
/// Like `post`, it is `ClientLocal` with an inert encoded action: `ChatViewService::invoke` runs
/// the verb itself on the workspace's open Chat record (`Conversations::retry`), and the record's
/// CRDT is what carries the retry to the person's other devices.
#[must_use]
pub(crate) fn retry_action() -> tilo_actions::ActionSpec {
    use tilo_actions::{ActionArgs, EncodedAction, ExecutionLocus, ParamType};
    tilo_actions::ActionSpec::typed(
        "retry",
        "Ask again for the answer",
        "a reply to ask again",
        ExecutionLocus::ClientLocal,
        vec![
            tilo_actions::ActionParam::required("channel", ParamType::Text, "The channel"),
            tilo_actions::ActionParam::required("reply", ParamType::Text, "The reply to ask again"),
            tilo_actions::ActionParam::required(
                "summons",
                ParamType::Text,
                "The message it answered",
            ),
        ],
        |_: &ActionArgs| EncodedAction::written("chat.retry".to_owned(), Vec::new()),
    )
}

/// The channel's `post` action, as the service publishes it on a channel's item: a message from
/// the person typing, in the channel it names — the very verb Chat's own composer runs. It is
/// the one definition of the verb a place's composer reaches; a service publishing a channel
/// offers it so the page and Chat's window post alike.
///
/// It is `ClientLocal` with an empty encoded action, and that is correct here: the dispatcher
/// never applies this verb's envelope. The session's `ChatViewService::invoke` runs the verb
/// itself — `Conversations::post` commits `ChatPart::POST` to the workspace's open Chat record
/// (`invoke` reads the invocation's `channel` and `text` directly), and the record's own CRDT is
/// what syncs the message to the person's other devices. So there is no client-local envelope to
/// replay with no text: the change that travels is the record's, and the encoded action is inert
/// on this path. A `ServerPreferred` verb would instead fail closed when no host is reachable,
/// which the composer must never do for a message the session can write itself.
#[must_use]
pub(crate) fn post_action() -> tilo_actions::ActionSpec {
    use tilo_actions::{ActionArgs, EncodedAction, ExecutionLocus, ParamType};
    tilo_actions::ActionSpec::typed(
        "post",
        "Post a message",
        "a message",
        ExecutionLocus::ClientLocal,
        vec![
            tilo_actions::ActionParam::required("channel", ParamType::Text, "The channel"),
            tilo_actions::ActionParam::required("text", ParamType::Text, "What it says"),
            tilo_actions::ActionParam::optional(
                "parent",
                ParamType::Text,
                "The message it answers, when it answers one",
            ),
            tilo_actions::ActionParam::optional(
                "span",
                ParamType::Text,
                "The run of the answered message it quotes, as start:end bytes",
            ),
        ],
        |_: &ActionArgs| EncodedAction::written("chat.post".to_owned(), Vec::new()),
    )
}

impl tilo_service::Service for ChatViewService {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        // Each conversation of each open workspace, as Chat publishes a channel: its item
        // resource, of the app `chat`, through its `page` view.
        let open = self.conversations.open.borrow();
        let mut specs: Vec<tilo_actions::ResourceSpec> = Vec::new();
        for opened in open.values() {
            let Some(chat_record) = &opened.chat else {
                continue;
            };
            let chat = chat_record.get();
            for (id, conversation) in chat.channels.iter() {
                // Every conversation the record holds is a place's resource: a channel of the
                // workspace, and the ask conversation a person and an agent talk in (a channel
                // the record keeps with its two participants, which `is_channel` calls no
                // workspace channel but which the canvas places all the same).
                //
                let spec =
                    channel_resource(id.as_str(), &conversation.name, conversation.is_channel());
                specs.push(spec);
            }
            // Each turn's summons, as Chat publishes a message's item: the message a person
            // asked the agent in, which Chat's own `record-work` writes the answer to. The
            // agent's address *is* that answer's — derived from the summons and the agent
            // (`chat::record::reply_id`) before either exists — so the summons's item carries
            // the verb from the moment the turn does, never only once Chat's window publishes
            // the message. The item is Chat's own, published through `ChatPart`'s declaration,
            // and the verb runs on Chat's record: a verb reimplemented here would be a second
            // chat. It is unbound, as Chat's window's item is: the agent names the message whose
            // work it records, exactly as it does on a message's own item the window publishes.
            let Some((index, part)) = &opened.part else {
                continue;
            };
            let revision = (chat_record.revision(), part.revision(), *index);
            let kept = self
                .summonses
                .borrow()
                .as_ref()
                .and_then(|(at, items)| (*at == revision).then(|| items.clone()));
            let items = kept.unwrap_or_else(|| {
                let built: Vec<tilo_actions::ResourceSpec> = summonses(&chat, &part.get())
                    .into_iter()
                    .map(|summons| summons_item(&summons))
                    .collect();
                *self.summonses.borrow_mut() = Some((revision, built.clone()));
                built
            });
            specs.extend(items);
        }
        specs
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        let channel = resource
            .strip_prefix("chat.workspace/channels/")
            .or_else(|| resource.strip_prefix("chat.workspace.channels:"))?;
        let reading = self.conversations.channel_reading(channel)?;
        serde_json::to_value(&reading).ok()
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        if action == "post" {
            let channel = arguments
                .get("channel")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    resource
                        .strip_prefix("chat.workspace/channels/")
                        .or_else(|| resource.strip_prefix("chat.workspace.channels:"))
                })
                .unwrap_or_default();
            let text = arguments
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if text.trim().is_empty() {
                return Err(tilo_actions::DoorError::InvalidArguments(
                    "a message says something".to_owned(),
                ));
            }
            let parent = arguments
                .get("parent")
                .and_then(serde_json::Value::as_str)
                .filter(|parent| !parent.is_empty());
            // A span the reply answers: `<start>:<end>`, UTF-8 bytes of the answered message, as
            // the selectable message body reports them. Its words are read from that message
            // where the reply is written, so the quote carries what was chosen.
            let span = arguments
                .get("span")
                .and_then(serde_json::Value::as_str)
                .and_then(span_of);
            if span.is_some() && parent.is_none() {
                return Err(tilo_actions::DoorError::InvalidArguments(
                    "a quoted span answers a message".to_owned(),
                ));
            }
            self.conversations
                .post_span(channel, text, parent, span)
                .map_err(tilo_actions::DoorError::Failed)?;
            return Ok(tilo_service::Started::Done(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // A turn's reply records its work in Chat's own words: the item is Chat's own, and the
        // verb is run by Chat on Chat's record, attributed as the person here.
        if action == RECORD_WORK {
            let invoker = tilo_actions::ActionInvoker::actor(self.conversations.person());
            return self
                .conversations
                .record_work(&invoker, resource, arguments)
                .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()));
        }
        // A turn whose answer failed is asked again on the same message: Chat's own `retry`,
        // run on Chat's record by this session, attributed to the person pressing it — or to
        // the agent that sent it, from the REPL or a step.
        if action == RETRY {
            let invoker = tilo_actions::ActionInvoker::actor(self.conversations.person());
            return self
                .conversations
                .retry(&invoker, arguments)
                .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()));
        }
        // The page's other verbs (react, pin, edit) are Chat's own on the messages it shows;
        // the service publishes the composer's `post` that a place reaches, and the failure
        // row's `retry`. A verb it does not publish is refused, as the door refuses any address
        // no service offers.
        Err(tilo_actions::DoorError::Unknown(
            tilo_actions::ActionAddress::new(resource, action),
        ))
    }

    /// [`Self::invoke`] as `invoker` (the agent answering the turn, a script): a record of work
    /// is Chat's own verb, not a message's, and is written as the agent that sent it, exactly as
    /// Chat's window writes its reply as the agent (`chat`'s `author(cx)` names the invoker, and
    /// the reply is derived from it). Every other verb is the person's, as before.
    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        if action == RECORD_WORK {
            return self
                .conversations
                .record_work(invoker, resource, arguments)
                .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()));
        }
        if action == RETRY {
            return self
                .conversations
                .retry(invoker, arguments)
                .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()));
        }
        self.invoke(resource, action, arguments)
    }
}

/// The run `start:end` names, in UTF-8 bytes: how a message's selection reports the words a
/// reply answers. `None` when it is not two ordered numbers.
fn span_of(text: &str) -> Option<(usize, usize)> {
    let (start, end) = text.split_once(':')?;
    let (start, end) = (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?);
    (start < end).then_some((start, end))
}

/// What a turn's agent is asked: the person's words and, when the message answers a run of
/// another, the words it quotes above them. The reply's own quote is the conversation's record
/// of what it means; this puts it where the agent reads its turn, so an answer to "yes, that
/// one" names what "that one" is without making the agent open the record it is acting in.
///
/// A message that answers no span is its own words, exactly as before.
fn instruction_of(message: &chat::record::Message) -> String {
    let Some(quoted) = message
        .quoted
        .as_ref()
        .filter(|quoted| !quoted.text.trim().is_empty())
    else {
        return message.text.clone();
    };
    format!("> {}\n\n{}", quoted.text.trim(), message.text)
}

/// The message `resource` names when it is a message's item or its id: `chat.workspace/messages/
/// <id>`, the item's bound form `chat.workspace.messages:message:<id>`, the kind-prefixed spelling
/// Chat's window publishes (`chat.messages/messages/<id>`), or the bare `<id>` an agent names it
/// by. Every spelling names the same message.
fn message_of(resource: &str) -> Option<String> {
    for prefix in [
        "chat.workspace/messages/",
        "chat.messages/messages/",
        "chat.workspace.messages:messages:",
        "chat.workspace.messages:message:",
    ] {
        if let Some(id) = resource.strip_prefix(prefix)
            && !id.is_empty()
            && !id.contains('/')
        {
            return Some(id.to_owned());
        }
    }
    // A bare message id, as an agent names the summons in its `message` argument.
    let bare = !resource.is_empty()
        && resource.len() <= 256
        && !resource.contains(['/', ':', '#'])
        && !resource.chars().any(char::is_whitespace);
    bare.then(|| resource.to_owned())
}

/// Every summons whose reply has no item Chat's window publishes yet: the person's messages an
/// agent in the conversation has not answered, and that no held message of the part answers. The
/// reply each names is the one its id and the agent's derive (`chat::record::reply_id`), so this
/// returns the summonses — the item the agent's verb is offered on — never the reply.
/// The summonses whose turn has no reply yet: the person's messages in a conversation an agent
/// takes part in, which no held message answers. The item each is published as carries Chat's
/// `record-work`, its message bound, so the agent's verb is offered on the address it answers from
/// before the reply the verb makes exists — the reply's own item is Chat's window's once it does.
/// The summons whose turn is going: the person's messages in a conversation an agent takes part
/// in, whose reply the agent has not finished — absent yet, or still working (no state that ends
/// it) — or whose reply the person asked again (`chat`'s `retry`, which gives the reply up). The
/// same rule the pass reads turns by, so the item that records the work is offered exactly while
/// the agent answers there: from the moment the turn exists, through every record it makes on the
/// reply it is writing, and again when a failed reply was retried.
fn summonses(chat: &chat::Chat, part: &chat::parts::ChatPart) -> Vec<Id> {
    let mut out: Vec<Id> = Vec::new();
    for (id, message) in part.messages.iter() {
        if message.text.trim().is_empty() || person_of(&message.author.0).is_none() {
            continue;
        }
        if chat
            .channels
            .get(&message.channel)
            .is_some_and(|conversation| conversation.archived)
        {
            continue;
        }
        let agents: Vec<String> = chat
            .conversation_participants(&message.channel)
            .into_iter()
            .filter_map(|participant| match &participant {
                // The reply an agent's answer is derives from the participant key it writes as,
                // which spells the agent `agent:<id>` (`chat::record::reply_id`).
                chat::record::Participant::Agent(_) => Some(participant.key()),
                chat::record::Participant::Person(_) => None,
            })
            .collect();
        for agent in agents {
            // The reply of the attempt the message is in: a retry moved it on, so the reply that
            // failed stands as history and the item offers the next attempt's address.
            let reply = chat::record::reply_id_at(id, &agent, message.attempts);
            let going = part
                .messages
                .get(&reply)
                // A reply without work is a plain answer; one that keeps work is going until
                // its work ends. The same rule `turns` reads the turn by.
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
                });
            if going && !out.contains(id) {
                out.push(id.clone());
            }
        }
    }
    out
}

/// The summons `id`, as Chat publishes a message's item: its id, its kind, its label, and Chat's
/// own `record-work` — the declaration `ChatPart::RECORD_WORK` publishes, whose action type is
/// Chat's, unbound so the agent names the message it records the work of as it does on Chat's
/// window's item. Built by [`tilo_resource::declaration_spec`], so the action an agent invokes is
/// Chat's verb, not a copy of it.
fn summons_item(id: &Id) -> tilo_actions::ResourceSpec {
    let kind = chat::parts::ChatPart::KIND;
    let mut spec = tilo_actions::ResourceSpec::new(
        format!("{kind}/messages/{id}"),
        "chat.messages",
        id.as_str(),
    )
    .with_type(tilo_actions::ResourceType {
        noun: tilo_actions::Noun::new("message", "messages"),
        app: tilo_actions::Noun::new("Chat", "Chat"),
        root: false,
    });
    spec.app_id = Some("chat".to_owned());
    let declaration = tilo_resource::vocabulary::<chat::parts::ChatPart>()
        .declarations()
        .iter()
        .find(|declaration| declaration.id == RECORD_WORK)
        .cloned();
    if let Some(declaration) = declaration {
        spec = spec.with_action(tilo_resource::declaration_spec(&declaration));
    }
    spec
}

/// The person a message's author names, when it names one: the participant key (`person:<root>`,
/// what a conversation's fields keep) or the bare id a record write attributes its change to
/// ([`ActionCx::author`](tilo_resource::action)). Both spell one person — the second is what Chat
/// writes a message's author as — so a message a person posted summons its conversation's agent
/// however it was written, and the run answers for the person it names.
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

/// The turns `chat` 's conversations give, read from `part` 's messages, and the agents the
/// conversations take in.
/// A conversation the pass has no message for is none: a direct conversation exists only as the id
/// its messages carry ([`chat::Chat::direct_id`]).
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
        // An archived conversation takes no message, and one the record does not list is a direct
        // conversation the record derives from its id.
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
            // The person's newest message the agent has not answered: the reply its id and the
            // agent's derive, which only the agent's answer to that message carries. An agent's
            // reply that is still working (its work is not finished) leaves the turn going, so a
            // runner that records "taken" before it acts is not read as having answered. A
            // message a person asked again (`chat`'s `retry`) is in a later attempt, whose reply
            // is absent yet: its turn goes again, and the answer is a reply of its own.
            let summons = messages
                .iter()
                .copied()
                .filter(|(message_id, message)| {
                    !message.text.trim().is_empty()
                        && person_of(&message.author.0).is_some()
                        && part
                            .messages
                            .get(&chat::record::reply_id_at(
                                message_id,
                                &key,
                                message.attempts,
                            ))
                            // A reply without work is a plain answer; one that keeps work is
                            // going until its work ends.
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
            // The conversation's address in the workspace, as the desk and the apps name it, and
            // the message's own beneath it.
            let address = format!("{}/channels/{id}", chat::Chat::KIND);
            turns.push(Entry {
                run: TrackerRun {
                    issue: format!("{address}/messages/{message_id}"),
                    title: chat.display_names(id, Some(person)),
                    // What the agent is asked: the person's words and, when the message answers
                    // a run of another, the words it quotes — the turn's message includes its
                    // quote, so an answer to "yes, that" names what "that" is.
                    instruction: instruction_of(message),
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

/// The session's Chat service, as a test drives it: what it publishes, and one of its actions
/// invoked as an agent. Kept beside the service it is about rather than in `lib.rs`, which holds
/// the session's own API.
impl crate::Session {
    /// Every resource the session's services publish now, each with its actions: what a test
    /// reads to prove a service offers a verb at an address before anything has made its resource
    /// exist.
    #[must_use]
    pub fn service_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.shared.services.borrow().resources()
    }

    /// The messages people wrote in `channel`, by their ids: what a test reads to prove a retry
    /// asked the agent again on the same message rather than posting the question a second time.
    #[must_use]
    pub fn person_messages_in(&self, channel: &str) -> Vec<String> {
        self.shared.conversations.person_messages(channel)
    }

    /// A step an address no surface door publishes, as the session's own `DoorAsk::Step` runs it:    /// a service's action, invoked through [`crate::agent_door::invoke_service_step`], on the
    /// record the service keeps. This is the exact call an agent's `record-work` on a turn's
    /// summons goes through at its broker step; a test drives it here rather than through the
    /// broker so it proves the seam without the agent.
    ///
    /// # Errors
    /// The service's refusal, or that no service publishes the address.
    pub fn invoke_service_step_as(
        &self,
        actor: &str,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        crate::agent_door::invoke_service_step(
            &self.shared.services,
            actor,
            &tilo_actions::ActionAddress::new(resource, action),
            arguments,
        )
    }
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

    /// A workspace holding one conversation between `ME` and `AGENT` .
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

    /// A message `author` wrote in `CONVERSATION` at `ts` .
    fn message(author: &str, text: &str, ts: i64) -> chat::record::Message {
        chat::record::Message {
            channel: Id::from_raw(CONVERSATION),
            author: Atom(author.into()),
            ts,
            text: text.into(),
            ..chat::record::Message::default()
        }
    }

    /// The head part of the messages, holding `messages` .
    fn part(messages: Vec<(&str, chat::record::Message)>) -> chat::parts::ChatPart {
        let mut part = chat::parts::ChatPart::default();
        for (id, message) in messages {
            part.messages.push(Id::from_raw(id), message);
        }
        part
    }

    /// The turns `chat` 's conversations give for the messages of `head` .
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

    /// A reply whose turn failed and that the person asked again no longer answers: the same
    /// message is a turn again, in a later attempt whose reply is its own.
    #[test]
    fn a_retried_reply_gives_its_summons_a_turn_again() {
        let chat = workspace();
        let mut asked = message(ME, "add the revenue curve", 10);
        let mut head = part(vec![("m1", asked.clone())]);
        let mut failed = message(AGENT_ACTOR, "", 20);
        failed.parent = Some(Id::from_raw("m1"));
        failed.work = Some(chat::record::Work {
            state: Some(chat::record::Transition {
                state: chat::model::AgentWorkState::ProviderFinished {
                    outcome: chat::agent_stream::AgentFinish::Failed,
                },
                since: 20,
            }),
            ..chat::record::Work::default()
        });
        head.messages.push(
            chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR),
            failed,
        );
        assert_eq!(
            read(&chat, &head),
            Vec::new(),
            "a failed reply ends the turn until the person asks again"
        );
        // The retry moved the question to its next attempt: its reply is the one derived for
        // that attempt, and nobody has written it.
        asked.attempts = 1;
        head.messages.push(Id::from_raw("m1"), asked.clone());
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("the retried summons is a turn again: {entries:?}");
        };
        assert_eq!(entry.run.instruction, "add the revenue curve");
        assert_eq!(
            entry.run.turn,
            Some(Turn::new("chat.workspace/channels/c_tide", "m1")),
            "the turn is the same message, never a second question"
        );
        assert_eq!(
            summonses(&chat, &head),
            vec![Id::from_raw("m1")],
            "the summons's item offers `record-work` again, so the agent's new answer lands"
        );
        // The answer lands in the attempt's own reply, never the one that failed.
        let next = chat::record::reply_id_at(&Id::from_raw("m1"), AGENT_ACTOR, asked.attempts);
        assert_ne!(
            next,
            chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR),
            "a retried turn is answered in a reply of its own"
        );
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

    #[test]
    fn a_reply_to_a_span_reaches_its_agents_turn_with_the_span() {
        // The person answered a run of what the agent wrote ("yes, that one"): the turn's message
        // carries the words they chose, so the agent's instruction names what "that one" is — the
        // reply's own words and the run it quotes, never a bare "yes".
        let chat = workspace();
        let answered = "The curve is flat from April to June, then climbs.";
        let chosen = "flat from April to June";
        let start = answered.find(chosen).expect("the words are in the message");
        let end = start + chosen.len();
        let mut reply = message(ME, "yes, that stretch", 20);
        reply.parent = Some(Id::from_raw("m1"));
        reply.quoted = Some(chat::record::Quote {
            text: chosen.to_owned(),
            start: u64::try_from(start).expect("an offset"),
            end: u64::try_from(end).expect("an offset"),
        });
        let head = part(vec![
            ("m1", message(AGENT_ACTOR, answered, 10)),
            ("m2", reply),
        ]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert!(
            entry.run.instruction.contains(chosen),
            "the turn's message carries the quoted run: {:?}",
            entry.run.instruction
        );
        assert!(
            entry.run.instruction.contains("yes, that stretch"),
            "and the person's own words: {:?}",
            entry.run.instruction
        );
        // A message that answers no span is its own words, exactly as before.
        let plain = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        assert_eq!(
            read(&chat, &plain)
                .first()
                .map(|entry| entry.run.instruction.clone()),
            Some("add the revenue curve".to_owned())
        );
    }
}
