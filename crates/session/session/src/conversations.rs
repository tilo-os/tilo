//! The turns a person's conversations give this session's agents.
//! An agent works inside a turn: a person's message in a conversation the agent takes part in
//! ([`tilo_action_policy::turn`]). Each turn is answered in the agent's session for that
//! conversation ([`crate::agent_sessions`]): the broker invokes the agent's answer with it.
//! Nothing here keeps a record of its own: the conversation's own record says what is asked and
//! what was answered. A message summons its conversation's agents, and an agent's answer is the
//! reply with the id its summons and its id derive ([`chat::record::reply_id`]), so a turn is over
//! when its reply exists and a later message is a turn of its own.
//! Conversations are items of Chat's workspace record and their messages are its parts
//! ([`chat::parts`]), so this reads Chat's record through the same store every app of the workspace
//! opens, as the workspace's settings tabs read the workspace's agents.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_action_policy::WorkspacePolicy;
use tilo_action_policy::turn::Turn;
use tilo_app::framework::WorkspaceRecord;
use tilo_broker_core::services::AgentTurn;
use tilo_resource::{Id, Resource as _};
use ui::permissions::{LivePermissions, MemberId, PermissionContext};
use ui::record::RecordId;
use ui::record::workspace::WorkspaceRecords;

/// The app that keeps the conversations: its own record of a workspace.
const CHAT_OWNER: &str = "so.tilo.chat";
/// How the run source names Chat where a record is opened.
const CHAT_APP: (&str, &str) = ("chat", "Chat");
/// Chat's verb that records a turn's work on the reply to its message: the one Chat's window
/// publishes on each message, run here on the same record from the same declaration.
const RECORD_WORK: &str = "record-work";

/// One workspace as the pass hands it over: the grants its turns are decided against.
pub(crate) struct WorkspaceFacts {
    pub(crate) workspace: String,
    pub(crate) policy: WorkspacePolicy,
}

/// The conversations this session's agents answer in, read from Chat's record of each workspace it
/// holds.
pub(crate) struct Conversations {
    /// The agent sessions the turns are answered in.
    sessions: Arc<crate::agent_sessions::AgentSessions>,
    /// The workspaces' records, as the workspace holds them.
    records: crate::engine_records::EngineRecords,
    /// Wakes the session's loop when a change to a record lands.
    wake: ui::WakeScope,
    /// Each workspace's records, opened here and followed change by change.
    open: RefCell<BTreeMap<String, Opened>>,
    /// The turns asked from the canvas that stood, by the message that asked, with the
    /// conversation it is in and what it said: the turns, and their conversations, this session
    /// publishes for their places to draw ([`TurnSummonses`]).
    asked: RefCell<BTreeMap<String, (String, String)>>,
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
    /// The conversations in the workspaces' copy of them (`records`), waking the session's loop through `wake` when a
    /// change lands.
    pub(crate) fn new(
        records: crate::engine_records::EngineRecords,
        sessions: Arc<crate::agent_sessions::AgentSessions>,
        wake: &tilo_service::Wake,
    ) -> Self {
        let mut scope = ui::WakeScope::new();
        let waking = Arc::clone(wake);
        if let Err(error) = scope.bind(move |_| waking()) {
            tracing::warn!(%error, "the conversations are not woken");
        }
        Self {
            sessions,
            records,
            wake: scope,
            open: RefCell::default(),
            asked: RefCell::default(),
        }
    }

    /// The person seated here: whose changes what the service records are theirs, and the author
    /// a message or a record of work is written by when the caller names nobody else.
    #[must_use]
    pub fn person(&self) -> String {
        let open = self.open.borrow();
        open.values()
            .next()
            .map(|opened| opened.records.author().key())
            .unwrap_or_default()
    }

    /// The workspace's agents by their id, as the workspace keeps their names: what a
    /// conversation with an agent is named for.
    fn agent_names(opened: &Opened) -> chat::projection::AgentNames {
        Self::workspace_agents(opened)
            .map(|record| record.get().active_names())
            .unwrap_or_default()
    }

    /// The workspace's agents record `opened` holds, opened here where it is not. `None` where the
    /// workspace keeps none — reading an app's record creates none, as [`Self::opened`] reads
    /// Chat's.
    fn workspace_agents(
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
            opened.records.author().key(),
        );
        let permissions = PermissionContext::from_standing(standing)
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        WorkspaceRecord::<tilo_app::tilo_workspace_agent::WorkspaceAgents>::open(
            &opened.records,
            AGENTS_OWNER,
            CHAT_APP,
            Some(LivePermissions::fixed(permissions)),
            None,
        )
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

    /// Record on `turn` 's message that `agent` stopped before it answered it, through Chat's own
    /// `record-work` as the agent records its work: the person reads the failure on the reply and
    /// can ask the message again from there. Nothing is written over an answer the agent gave.
    pub(crate) fn turn_failed(&self, agent: &str, turn: &Turn, attempt: u32, reason: &str) {
        let summons = Id::from_raw(turn.message.clone());
        let reply = chat::record::reply_id_at(&summons, &MemberId::agent(agent).key(), attempt);
        // `Some(true)` where a workspace open here holds the message and has no answer for it
        // yet: anywhere else there is nothing to write the failure to.
        let record = {
            let open = self.open.borrow();
            open.values().find_map(|opened| {
                let (_, part) = opened.part.as_ref()?;
                opened.chat.as_ref()?;
                let part = part.get();
                // Asked again since: the failure belongs to the attempt it answered, which no
                // message of the conversation waits on now.
                let message = part.messages.get(&summons)?;
                (message.attempts == attempt).then(|| unanswered(part.messages.get(&reply)))
            })
        };
        if record != Some(true) {
            return;
        }
        let arguments = serde_json::Map::from_iter([
            ("message".to_owned(), serde_json::json!(turn.message)),
            (
                "work".to_owned(),
                serde_json::json!({ "state": "refused", "detail": reason }),
            ),
            (
                "say".to_owned(),
                serde_json::json!(crate::i18n::t("turn-unanswered")),
            ),
        ]);
        let invoker = tilo_actions::ActionInvoker::agent(agent);
        if let Err(error) = self.record_work(&invoker, &turn.message, &arguments) {
            tracing::warn!(%error, %agent, "a turn that ended without an answer is not recorded");
        }
    }

    /// Read each workspace's conversations as `person`, and hand the turns they give to the agent
    /// sessions.
    ///
    /// A pass that read different turns than the one before announces them
    /// ([`tilo_identity_host::policy_change::announce`]): the broker re-reads the turns when that
    /// changes, not on a clock, so a new turn — the person typing an ask into a conversation Chat
    /// had no message in — is only handed to its agent because this says it exists. Nothing is
    /// announced while the turns are the ones already read.
    pub(crate) fn feed(&self, person: &str, workspaces: Vec<WorkspaceFacts>) {
        let read = workspaces
            .into_iter()
            .map(|facts| {
                let turns = self.turns(&facts, person);
                (facts.workspace, turns)
            })
            .collect();
        if self.sessions.asked(read) {
            tilo_identity_host::policy_change::announce();
        }
    }

    /// The turns `workspace` 's conversations give, read at the revisions its records are at now,
    /// each with what its agent is called.
    fn turns(&self, workspace: &WorkspaceFacts, person: &str) -> Vec<(AgentTurn, String)> {
        match self.opened(workspace, person) {
            Some((chat, part)) => {
                // A conversation with an agent is named for it, as the window names it: the
                // workspace's own record of its agents says what it is called.
                let names = self
                    .open
                    .borrow()
                    .get(&workspace.workspace)
                    .map(Self::agent_names)
                    .unwrap_or_default();
                let chat_record = chat.get();
                let part_record = part.get();
                turns(
                    &workspace.workspace,
                    &chat_record,
                    &part_record,
                    person,
                    &names,
                )
            }
            None => Vec::new(),
        }
    }

    /// The workspace's chat record and the head of its messages, opened here where they are not.
    /// A workspace whose person never opened Chat holds no record, and the reader creates none.
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
            records: self
                .records
                .in_workspace(&workspace.workspace, MemberId::person(person.to_owned())),
            chat: None,
            part: None,
        });
        let permissions = PermissionContext::from_standing(tilo_action_policy::Standing::person(
            workspace.policy.clone(),
            person.to_owned(),
        ))
        .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        let chat_id = RecordId::package_primary();
        if opened.chat.is_none() {
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

    /// The turn the message `question` opened — the one Chat said it posted for an ask the person
    /// sent from the canvas — once Chat's record of it is here (canvas.md §3): its conversation,
    /// and the turn as this session publishes it from then on ([`TurnSummonses`]), read from
    /// Chat's record as Chat posted it. `None` while the record does not hold the message yet.
    pub(crate) fn asked_turn(
        &self,
        question: &str,
    ) -> Option<(
        tilo_session_core::layout::Asked,
        Vec<tilo_actions::ResourceSpec>,
    )> {
        let (channel, text) = {
            let open = self.open.borrow();
            open.values().find_map(|opened| {
                let (_, part) = opened.part.as_ref()?;
                let part = part.get();
                part.messages
                    .iter()
                    .find(|(id, _)| id.as_str() == question)
                    .map(|(_, held)| (held.channel.as_str().to_owned(), held.text.clone()))
            })?
        };
        self.asked
            .borrow_mut()
            .insert(question.to_owned(), (channel.clone(), text.clone()));
        Some((
            tilo_session_core::layout::Asked {
                conversation: format!("{}/channels/{channel}", chat::Chat::KIND),
                message: question.to_owned(),
                turn: turn_address(question),
                opened_as: format!("{CHANNELS}{channel}"),
            },
            vec![
                turn_resource(question, &text),
                conversation_resource(&channel, self.conversation_name(&channel)),
            ],
        ))
    }

    /// The turn the message `question` opened, as its place draws it (canvas.md §3): the message
    /// and what the agent answered, read from Chat's record of the conversation holding it, as
    /// Chat's own page reads it. `None` where no open workspace holds the message.
    #[must_use]
    pub fn turn_reading(&self, question: &str) -> Option<chat::views::Turn> {
        let channel = self.asked.borrow().get(question)?.0.clone();
        let page = self.channel_reading(&channel)?;
        chat::views::Turn::of(&page, question)
    }

    /// What the conversation `channel` is called, as Chat's window names it: the agent's name for
    /// a conversation with an agent.
    fn conversation_name(&self, channel: &str) -> String {
        let open = self.open.borrow();
        open.values()
            .find_map(|opened| {
                let chat = opened.chat.as_ref()?.get();
                let person = opened.records.author().key();
                Some(chat.display_names(
                    &Id::from_raw(channel),
                    Some(&person),
                    &Self::agent_names(opened),
                ))
            })
            .unwrap_or_default()
    }

    /// The conversation `channel` (its id, a direct conversation's included) as Chat's own page
    /// draws it, read from Chat's record of the workspace this session holds open that has its
    /// messages: Chat's projection of its own record, the rows its window shows. `None` where no
    /// open workspace holds a message of it.
    #[must_use]
    pub fn channel_reading(&self, channel: &str) -> Option<chat::views::Channel> {
        let open = self.open.borrow();
        for opened in open.values() {
            let (Some(chat_record), Some((index, part))) = (&opened.chat, &opened.part) else {
                continue;
            };
            let held = chat::parts::ChatPart::clone(part.get().as_ref());
            if !held
                .messages
                .iter()
                .any(|(_, message)| message.channel.as_str() == channel)
            {
                continue;
            }
            let chat = chat_record.get();
            let messages = chat::messages::Messages::new(vec![(*index, std::rc::Rc::new(held))]);
            let rows = chat.message_rows(&messages, channel);
            let me = opened.records.author().key();
            let names = chat::views::Names {
                me: crate::platform::person::person_name(
                    ui::local_identity::load().map(|identity| identity.display_name),
                    &me,
                ),
                agents: Self::agent_names(opened),
            };
            return Some(chat::views::Channel::read(
                &chat, &rows, channel, &me, &names,
            ));
        }
        None
    }

    /// The revision of every open workspace's chat and messages: what a turn's reading is read
    /// at, so a reply landing reads it again.
    fn revisions(&self) -> Vec<(u64, u64, u64)> {
        let open = self.open.borrow();
        open.values()
            .filter_map(|opened| {
                let chat = opened.chat.as_ref()?;
                let (index, part) = opened.part.as_ref()?;
                Some((chat.revision(), part.revision(), *index))
            })
            .collect()
    }
}

/// Where a turn is published: its stable address `chat.workspace.turns:<message>`, by the
/// message that asked.
const TURNS: &str = "chat.workspace.turns:";

/// The turn the message `question` opened, by its address.
fn turn_address(question: &str) -> String {
    format!("{TURNS}{question}")
}

/// What a turn offers on its place's handle: the whole conversation it was asked in, opened
/// beside it (canvas.md §7, "the conversation is reached from the handle").
const CONVERSATION: &str = "conversation";

/// [`CONVERSATION`] as the turn publishes it: an action with no arguments, which the session runs
/// itself ([`TurnSummonses::invoke_as`]).
fn conversation_action() -> tilo_actions::ActionSpec {
    tilo_actions::ActionSpec::typed_operation(
        CONVERSATION,
        "Whole conversation",
        "Opens the whole conversation this answer was asked in, beside it.",
        tilo_actions::OperationKind::Effect,
        chat::views::TURN_KIND,
        Vec::new(),
        |_| tilo_actions::EncodedAction {
            type_id: chat::views::TURN_KIND.into(),
            payload: CONVERSATION.as_bytes().to_vec(),
        },
    )
}

/// One turn as a place's resource (canvas.md §3): the message `question` a person asked an agent
/// in, named by what they asked, drawn through Chat's `turn` view.
fn turn_resource(question: &str, asked: &str) -> tilo_actions::ResourceSpec {
    let label = asked.lines().next().unwrap_or_default().trim();
    let mut spec =
        tilo_actions::ResourceSpec::new(turn_address(question), chat::views::TURN_KIND, label)
            .with_view(tilo_actions::ResourceViewSpec::new(
                tilo_session_core::layout::TURN_VIEW,
                label.to_owned(),
                "open",
            ))
            .with_type(tilo_actions::ResourceType {
                noun: tilo_actions::Noun::new("answer", "answers"),
                app: tilo_actions::Noun::new("Chat", "Chat"),
                root: false,
            })
            .with_action(conversation_action());
    spec.app_id = Some("chat".to_owned());
    spec
}

/// Where a conversation is published: Chat's channel item at its stable address
/// (`chat.workspace.channels:<id>`), a direct conversation's included.
const CHANNELS: &str = "chat.workspace.channels:";

/// The conversation `channel` as a place's resource: Chat's channel, through Chat's own page,
/// with Chat's own `send` its composer runs. What a turn's "Whole conversation" places beside it.
fn conversation_resource(channel: &str, label: String) -> tilo_actions::ResourceSpec {
    let mut spec = tilo_actions::ResourceSpec::new(
        format!("{CHANNELS}{channel}"),
        "chat.workspace.channels",
        label.clone(),
    )
    .with_view(tilo_actions::ResourceViewSpec::new("page", label, "open"))
    .with_type(tilo_actions::ResourceType {
        noun: tilo_actions::Noun::new("conversation", "conversations"),
        app: tilo_actions::Noun::new("Chat", "Chat"),
        root: false,
    });
    if let Some(send) =
        <chat::effects::ChatEffect as tilo_resource::Effect>::spec(crate::send_to::SEND)
    {
        spec = spec.with_action(send);
    }
    spec.app_id = Some("chat".to_owned());
    spec
}

/// The turns going in the workspaces' conversations as a session service: each turn's summons, as
/// Chat publishes a message's item, carrying Chat's own `record-work`, so the agent answering it has
/// the address its answer goes to whether or not Chat's window is open. The verb runs on the
/// workspace's open Chat record, the same record the pass reads turns from — never a second writer
/// on the store.
pub(crate) struct TurnSummonses {
    conversations: Rc<Conversations>,
    /// The summonses it published last, keyed by each open chat's revision and messages' part:
    /// rebuilt only when a part moved or a workspace opened, so a pass that changed nothing
    /// writes no item again — and reads no part, which would cost the loop every frame.
    summonses: std::cell::RefCell<Option<(SummonsKey, Vec<tilo_actions::ResourceSpec>)>>,
    /// The revisions the asked turns were last read at: a reply landing moves them, and the turns'
    /// places read again.
    turns_at: Vec<(u64, u64, u64)>,
    /// The one send: what a conversation's composer sends goes through it to Chat.
    sends: crate::send_to::Sends,
    /// The layout: where a turn's whole conversation is opened beside it.
    layout: Rc<RefCell<tilo_session_core::layout::Layout>>,
}

/// What the summonses a [session's summonses](TurnSummonses) publishes depend on: each open
/// chat's revision, its messages' head part's revision, and the part's number.
type SummonsKey = (u64, u64, u64);

impl TurnSummonses {
    /// The service over `conversations`, the session's one open of the workspaces' chats.
    #[must_use]
    pub(crate) fn new(
        conversations: Rc<Conversations>,
        sends: crate::send_to::Sends,
        layout: Rc<RefCell<tilo_session_core::layout::Layout>>,
    ) -> Self {
        Self {
            conversations,
            summonses: std::cell::RefCell::new(None),
            turns_at: Vec::new(),
            sends,
            layout,
        }
    }
}

impl tilo_service::Service for TurnSummonses {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let open = self.conversations.open.borrow();
        let mut specs: Vec<tilo_actions::ResourceSpec> = Vec::new();
        for opened in open.values() {
            let Some(chat_record) = &opened.chat else {
                continue;
            };
            let chat = chat_record.get();
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
        // Each turn asked from the canvas, as its place draws it (canvas.md §3), and the
        // conversation it is in, which the turn places beside it on request.
        let asked = self.conversations.asked.borrow();
        let mut channels: Vec<&str> = Vec::new();
        for (question, (channel, text)) in asked.iter() {
            specs.push(turn_resource(question, text));
            if !channels.contains(&channel.as_str()) {
                channels.push(channel);
            }
        }
        specs.extend(channels.into_iter().map(|channel| {
            conversation_resource(channel, self.conversations.conversation_name(channel))
        }));
        specs
    }

    /// A summons has no reading of its own: its message is Chat's to show. A turn asked from the
    /// canvas reads as the question and what the agent answered, from Chat's record.
    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        if let Some(question) = resource.strip_prefix(TURNS) {
            let reading = self.conversations.turn_reading(question)?;
            return serde_json::to_value(&reading).ok();
        }
        let channel = resource.strip_prefix(CHANNELS)?;
        let reading = self.conversations.channel_reading(channel)?;
        serde_json::to_value(&reading).ok()
    }

    /// A reply landing in Chat's record moves the turns asked from the canvas: their places read
    /// them again.
    fn poll(&mut self) -> bool {
        if self.conversations.asked.borrow().is_empty() {
            return false;
        }
        let at = self.conversations.revisions();
        if at == self.turns_at {
            return false;
        }
        self.turns_at = at;
        true
    }

    /// A record of work asked with no invoker is the person's here.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        let invoker = tilo_actions::ActionInvoker::person(self.conversations.person());
        self.invoke_as(&invoker, resource, action, arguments)
    }

    /// A turn's reply records its work in Chat's own words, as `invoker` (the agent answering the
    /// turn, a script): the item is Chat's own, and the verb is run on Chat's record, written as
    /// the agent that sent it, exactly as Chat's window writes its reply as the agent (`chat`'s
    /// `author(cx)` names the invoker, and the reply is derived from it). Every other verb on a
    /// message is Chat's own, published by Chat, and refused here.
    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        // A conversation's composer sends through the one send, to the conversation it is: Chat
        // posts it, as a box's message.
        if let Some(channel) = resource.strip_prefix(CHANNELS)
            && action == crate::send_to::SEND
        {
            let text = arguments
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let to = ui::floating_surface::Recipient::found(
                ui::floating_surface::RecipientKind::Conversation,
                channel.to_owned(),
                String::new(),
            );
            let mut send = serde_json::Map::new();
            send.insert("to".into(), serde_json::json!([to]));
            send.insert("text".into(), text.into());
            return self.sends.send(Some(invoker.clone()), &send);
        }
        // A turn's whole conversation, from its place's handle: opened beside the turn, as the
        // person's (or the invoker's) own arranging step.
        if action == CONVERSATION && resource.starts_with(TURNS) {
            let actor = invoker.key();
            let asked = if actor.is_empty() {
                self.conversations.person()
            } else {
                actor
            };
            self.layout
                .borrow_mut()
                .open_conversation_of(&asked, resource)
                .map_err(|why| tilo_actions::DoorError::Unavailable {
                    code: "nowhere_to_open".to_owned(),
                    reason: why,
                })?;
            return Ok(tilo_service::Started::Done(
                tilo_actions::ActionOutput::default(),
            ));
        }
        if action != RECORD_WORK {
            return Err(tilo_actions::DoorError::Unknown(
                tilo_actions::ActionAddress::new(resource, action),
            ));
        }
        self.conversations
            .record_work(invoker, resource, arguments)
            .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()))
    }
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

/// Whether `reply` has yet to answer its summons: absent, or still working. A reply without work is
/// a plain answer; one that keeps work is going until its work ends.
fn unanswered(reply: Option<&chat::record::Message>) -> bool {
    reply.is_none_or(|reply| {
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
            // The reply an agent's answer is derives from the member key it writes as, which
            // spells the agent `agent:<id>` (`chat::record::reply_id`).
            .filter(|participant| !participant.is_person())
            .map(|participant| participant.key())
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

/// The person a message's author names, when it names one: a member key that is a person
/// ([`MemberId::key`], what a conversation's fields keep). A message a person posted summons its
/// conversation's agent, and the run answers for the person it names; software names nobody.
fn person_of(author: &str) -> Option<String> {
    tilo_actions::policy::MemberId::parse(author)
        .and_then(|member| member.person_id().map(str::to_owned))
        .filter(|person| !person.is_empty())
}

/// The turns `chat` 's conversations in `workspace` give, read from `part` 's messages, each with
/// what its agent is called.
/// A conversation the pass has no message for is none: a direct conversation exists only as the id
/// its messages carry ([`chat::Chat::direct_id`]).
fn turns(
    workspace: &str,
    chat: &chat::Chat,
    part: &chat::parts::ChatPart,
    person: &str,
    agent_names: &chat::projection::AgentNames,
) -> Vec<(AgentTurn, String)> {
    let mut conversations: Vec<Id> = Vec::new();
    for (_, message) in part.messages.iter() {
        if !conversations.contains(&message.channel) {
            conversations.push(message.channel.clone());
        }
    }
    let mut turns = Vec::new();
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
            if participant.is_person() {
                continue;
            }
            let key = participant.key();
            let agent = participant.id().to_owned();
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
                    (!message.text.trim().is_empty() || !message.attachments.is_empty())
                        && person_of(&message.author.0).is_some()
                        && unanswered(part.messages.get(&chat::record::reply_id_at(
                            message_id,
                            &key,
                            message.attempts,
                        )))
                })
                .max_by_key(|(message_id, message)| (message.ts, message_id.as_str()));
            let Some((message_id, message)) = summons else {
                continue;
            };
            let Some(starter) = person_of(&message.author.0) else {
                continue;
            };
            // The conversation's address in the workspace, as the shell and the apps name it, and
            // the message's own beneath it.
            let address = format!("{}/channels/{id}", chat::Chat::KIND);
            let (instruction, subject) = request(message);
            let name = agent_names
                .get(&agent)
                .cloned()
                .unwrap_or_else(|| agent.clone());
            turns.push((
                AgentTurn {
                    session: session_of(workspace, &address, &agent),
                    agent,
                    author: starter,
                    turn: Turn::new(address.clone(), message_id.to_string()),
                    // A person's Retry asks again for the same message, and that is a turn of its
                    // own: the one they gave up on reads as replaced, and the broker hands the agent
                    // the new one.
                    attempt: message.attempts,
                    request: runtime_collab::TurnRequest {
                        message: format!("{address}/messages/{message_id}"),
                        title: chat.display_names(id, Some(person), agent_names),
                        instruction,
                        subject,
                        spend: None,
                    },
                },
                name,
            ));
        }
    }
    turns
}

/// The agent session `agent` answers the turns of the conversation at `conversation` in
/// `workspace` in: one per conversation and agent, the same on every pass.
fn session_of(workspace: &str, conversation: &str, agent: &str) -> String {
    let digest = blake3::hash(format!("{workspace}\0{conversation}\0{agent}").as_bytes());
    format!(
        "{}{}",
        tilo_broker_core::services::AGENT_SESSION_PREFIX,
        digest.to_hex().get(..32).unwrap_or_default()
    )
}

/// What a message asks of the agents it summons: its words, and what it carries as the records
/// they name — the first of them the run's subject, which the agent acts on through that record's
/// own actions, and every other one said beside the words. A file or words quoted from nowhere are
/// said as what they are.
fn request(message: &chat::record::Message) -> (String, Option<tilo_actions::SelectionSubject>) {
    let mut subject = None;
    // A reply that answers a run of another message is asked with the words it quotes above its
    // own: the reply's quote is the conversation's record of what it means, so an answer to "yes,
    // that one" names what "that one" is without the agent opening the record it acts in.
    let mut instruction = message
        .quoted
        .as_ref()
        .filter(|quoted| !quoted.text.trim().is_empty())
        .map_or_else(
            || message.text.trim().to_owned(),
            |quoted| format!("> {}\n\n{}", quoted.text.trim(), message.text.trim()),
        );
    for attached in &message.attachments {
        let named = attached.subject();
        if subject.is_none() && named.is_some() {
            subject = named;
            continue;
        }
        let line = match (named, attached) {
            (Some(named), _) => format!(
                "It also carries {:?} ({} `{}`, in `{}`).",
                attached.label(),
                named.resource.kind,
                named.resource.id,
                named.app_id
            ),
            (None, tilo_reference::Attached::Text { text, .. }) => {
                format!("It quotes: {text:?}.")
            }
            (None, _) => format!("It carries the file {:?}.", attached.label()),
        };
        if !instruction.is_empty() {
            instruction.push('\n');
        }
        instruction.push_str(&line);
    }
    (instruction, subject)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Atom;

    /// A turn offers its whole conversation as its own action, which its place's handle lists
    /// (canvas.md §7): the card itself holds no button for it.
    #[test]
    fn a_turn_offers_its_whole_conversation() {
        let turn = turn_resource("q1", "what is due?");
        let offered = turn
            .actions
            .iter()
            .find(|action| action.id == super::CONVERSATION)
            .expect("the turn offers its conversation");
        assert_eq!(offered.label, "Whole conversation");
        assert!(offered.params().is_empty(), "it takes nothing");
    }

    /// The agent the tests' conversations take in.
    const AGENT: &str = "tide";
    /// The id its words are written under.
    const AGENT_ACTOR: &str = "agent:tide";
    /// The person sitting here, by their member key.
    const ME: &str = "me";
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
    fn read(chat: &chat::Chat, head: &chat::parts::ChatPart) -> Vec<AgentTurn> {
        turns("w1", chat, head, ME, &chat::projection::AgentNames::new())
            .into_iter()
            .map(|(turn, _)| turn)
            .collect()
    }

    tilo_test_env::scenario! {
        fn a_conversations_messages_become_its_agents_turns;

        phase an_unanswered_message_is_a_turn_of_the_conversations_agent;
        {
            let chat = workspace();
            let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
            let entries = read(&chat, &head);
            let [entry] = entries.as_slice() else {
                panic!("one turn, not {entries:?}");
            };
            assert_eq!(entry.agent, AGENT);
            assert_eq!(entry.author, "me");
            assert_eq!(entry.request.instruction, "add the revenue curve");
            assert_eq!(entry.request.title, "Tide");
            assert_eq!(
                entry.turn,
                Turn::new("chat.workspace/channels/c_tide", "m1")
            );
        }

        phase what_a_message_carries_is_the_subject_of_its_turn;
        // What a send-to box carried reaches the agent as the record it names: the run's subject is
        // that record, to act on through its own actions, and a message carrying it with no words is
        // a turn too.
        {
            let chat = workspace();
            let issue = tilo_reference::ResourceAt {
                app: "so.tilo.tasks".into(),
                record: None,
                reference: tilo_reference::ResourceRef::whole(
                    "tasks.issue",
                    "tasks.tracker/issues/42",
                    "Launch checklist",
                ),
            };
            let mut handed = message(ME, "", 10);
            handed.attachments = vec![
                tilo_reference::Attached::resource(
                    issue,
                    Some(tilo_reference::Anchor::item("42")),
                    "Launch checklist",
                ),
                tilo_reference::Attached::Text {
                    text: "before Friday".into(),
                    from: None,
                },
            ];
            let entries = read(&chat, &part(vec![("m1", handed)]));
            let [entry] = entries.as_slice() else {
                panic!("a message carrying a task alone is a turn, not {entries:?}");
            };
            let subject = entry.request.subject.as_ref().expect("the task is the subject");
            assert_eq!(subject.app_id, "so.tilo.tasks");
            assert_eq!(subject.resource.id, "tasks.tracker/issues/42");
            assert_eq!(
                subject.anchor.place,
                Some(tilo_actions::SelectionPlace::item("42"))
            );
            assert_eq!(entry.request.instruction, "It quotes: \"before Friday\".");
        }

        phase a_persons_message_is_a_turn;
        // A person writes messages under their root key, which is the member key their
        // conversation keeps: a message they post summons its conversation's agent.
        {
            let bare = "22503e79b0b80274014ec6333d3561ca89cb3209937b901c4bc2e4bedccc2f85";
            let chat = workspace();
            let head = part(vec![("m1", message(bare, "what is on my task list?", 10))]);
            let entries = read(&chat, &head);
            let [entry] = entries.as_slice() else {
                panic!("one turn, not {entries:?}");
            };
            assert_eq!(entry.agent, AGENT);
            assert_eq!(entry.author, bare, "the turn acts for that person");
            assert_eq!(entry.request.instruction, "what is on my task list?");
            assert_eq!(
                person_of(AGENT_ACTOR),
                None,
                "an agent's message is no person's turn"
            );
        }

        phase the_agents_reply_ends_its_turn;
        {
            let chat = workspace();
            let reply = chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR);
            let mut head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
            head.messages.push(reply, message(AGENT_ACTOR, "done", 20));
            assert_eq!(read(&chat, &head), Vec::new());
        }

        phase the_newest_unanswered_message_is_the_turn;
        {
            let chat = workspace();
            let head = part(vec![
                ("m1", message(ME, "first", 10)),
                ("m2", message(ME, "second", 20)),
            ]);
            let entries = read(&chat, &head);
            let [entry] = entries.as_slice() else {
                panic!("one turn, not {entries:?}");
            };
            assert_eq!(entry.request.instruction, "second");
        }

        phase an_agents_own_words_are_no_summons;
        {
            let chat = workspace();
            let head = part(vec![("m1", message(AGENT_ACTOR, "on it", 10))]);
            assert_eq!(read(&chat, &head), Vec::new());
        }

        phase a_channel_of_the_whole_workspace_summons_no_agent;
        {
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
    }

    tilo_test_env::scenario! {
        fn a_retry_gives_the_same_message_a_turn_and_a_run_again;

        phase a_retried_reply_gives_its_summons_a_turn_again;
        // A reply whose turn failed and that the person asked again no longer answers: the same
        // message is a turn again, in a later attempt whose reply is its own.
        {
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
            assert_eq!(entry.request.instruction, "add the revenue curve");
            assert_eq!(
                entry.turn,
                Turn::new("chat.workspace/channels/c_tide", "m1"),
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

        phase a_retried_turn_is_a_new_turn_of_the_same_message;
        // A retried turn is a turn of its own, not the one that failed read again: it is in the
        // attempt the message moved to. That is what makes it deliverable — the pass announces a
        // change only when the turns differ ([`crate::agent_sessions::AgentSessions::asked`]), so a
        // turn that read exactly like the failed one would be handed to nobody.
        {
            let chat = workspace();
            let asker = message(ME, "add the revenue curve", 10);
            let mut head = part(vec![("m1", asker.clone())]);
            // The turn going before the answer was tried: the one the agent was handed first.
            let first = read(&chat, &head);
            let [before] = first.as_slice() else {
                panic!("the ask is a turn: {first:?}");
            };
            assert_eq!(before.attempt, 0, "the first attempt");
            // The try failed: the failed reply ends the turn.
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
            assert_eq!(read(&chat, &head), Vec::new());
            // The person pressed Retry: the same message is a turn again, and it is a new one.
            let mut retried = asker;
            retried.attempts = 1;
            head.messages.push(Id::from_raw("m1"), retried);
            let again = read(&chat, &head);
            let [after] = again.as_slice() else {
                panic!("the retried summons is a turn again: {again:?}");
            };
            assert_eq!(
                after.request.message, before.request.message,
                "the retried turn is the same message's"
            );
            assert_eq!(after.session, before.session, "in the same session");
            assert_eq!(
                after.attempt, 1,
                "the retried turn is the attempt the message moved to"
            );
            assert_ne!(
                *after, *before,
                "the retried turn differs from the one that failed, so the pass announces it"
            );
            // And the pass reads that as a change to deliver, exactly as it does a new turn.
            let sessions = crate::agent_sessions::AgentSessions::new();
            let named = |turns: Vec<AgentTurn>| {
                vec![(
                    "w1".to_owned(),
                    turns.into_iter().map(|turn| (turn, AGENT.to_owned())).collect(),
                )]
            };
            assert!(sessions.asked(named(first)));
            assert!(
                sessions.asked(named(again)),
                "the retry is a change the broker is woken for"
            );
        }
    }

    /// A pass that read different turns than the one before is what wakes the broker to deliver a
    /// new one: the person typing an ask into a conversation Chat held no message in is only handed
    /// to its agent because the sessions say the turns changed.
    #[test]
    fn a_new_turn_is_a_change_and_the_same_turns_are_not() {
        let chat = workspace();
        let asked = part(vec![("m1", message(ME, "what is on my task list?", 10))]);
        let read = turns(
            "w1",
            &chat,
            &asked,
            ME,
            &chat::projection::AgentNames::new(),
        );
        let sessions = crate::agent_sessions::AgentSessions::new();
        let held = |turns: Vec<(AgentTurn, String)>| vec![("w1".to_owned(), turns)];
        assert!(
            sessions.asked(held(read.clone())),
            "a turn that was not there is a change to deliver"
        );
        assert!(
            !sessions.asked(held(read)),
            "the same turns read again are no change"
        );
        assert!(
            sessions.asked(held(Vec::new())),
            "a turn that ended is a change too: its answer stops"
        );
    }
}
