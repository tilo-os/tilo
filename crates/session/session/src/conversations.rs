//! The conversations of the workspaces this session holds, and the turns going in them.
//! An agent works inside a turn: a person's message in a conversation the agent takes part in
//! ([`tilo_action_policy::turn`]). The workspace's agent engine answers it; this session publishes
//! each going turn's item, on which the agent records its work while it answers.
//! Nothing here keeps a record of its own: the conversation's own record says what is asked and
//! what was answered. A message summons its conversation's agents, and an agent's answer is the
//! reply with the id its summons and its id derive ([`chat::chat::reply_id`]), so a turn is over
//! when its reply exists and a later message is a turn of its own.
//! Conversations are items of Chat's workspace record and their messages are its parts
//! ([`chat::parts`]), so this reads Chat's record through the same store every app of the workspace
//! opens, as the workspace's settings tabs read the workspace's agents.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use chat::projection::ChatProjection as _;
use tilo_action_policy::WorkspacePolicy;
use tilo_app::framework::WorkspaceResource;
use tilo_app::tilo_workspace_agent::learned::Learned;
use tilo_resource::{Id, Resource as _};
use ui::permissions::{LivePermissions, MemberId, PermissionContext};
use ui::resource::DocumentId;
use ui::resource::workspace::WorkspaceResources;

/// The app that keeps the conversations: its own record of a workspace.
use ui::apps::CHAT as CHAT_OWNER;
/// How the run source names Chat where a record is opened.
const CHAT_APP: (&str, &str) = ("chat", "Chat");
/// Chat's verb that records a turn's work on the reply to its message: the one Chat's window
/// publishes on each message, run here on the same record from the same declaration.
const RECORD_WORK: &str = "record-work";
/// Chat's verb that keeps a conversation's summary: the one its agent keeps while it answers a
/// turn there ([`chat::recall`]), run here on Chat's record as `record-work` is.
const KEEP_SUMMARY: &str = "keep-summary";
/// The verbs of the person's own record of what their agents learned about them
/// ([`tilo_app::tilo_workspace_agent::learned`]): an agent keeps, corrects and forgets a fact
/// while it answers a turn, on the turn's own item.
const LEARNED_VERBS: [&str; 3] = ["learn", "correct", "forget"];

mod personal_learned;

/// One workspace as the pass hands it over: the grants its conversations are opened with.
pub(crate) struct WorkspaceFacts {
    pub(crate) workspace: String,
    pub(crate) policy: WorkspacePolicy,
    /// What the workspace's member list calls its people and agents, as apps are told it.
    pub(crate) members: chat::projection::PeopleNames,
}

/// The conversations this session's agents answer in, read from Chat's record of each workspace it
/// holds.
pub(crate) struct Conversations {
    /// The workspaces' records, as the workspace holds them.
    records: crate::engine_records::EngineRecords,
    /// Where the person's own workspace keeps what their agents learned of them.
    personal: RefCell<Option<crate::PersonResources>>,
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
    records: WorkspaceResources,
    /// What the workspace's member list calls its people and agents, as the last pass read it.
    members: chat::projection::PeopleNames,
    chat: Option<WorkspaceResource<chat::Chat>>,
    /// The head of the messages' chain, and the part it holds here.
    part: Option<(u64, WorkspaceResource<chat::parts::ChatMessages>)>,
}

impl Conversations {
    /// The conversations in the workspaces' copy of them (`records`), waking the session's loop through `wake` when a
    /// change lands.
    pub(crate) fn new(
        records: crate::engine_records::EngineRecords,
        wake: &tilo_service::Wake,
    ) -> Self {
        let mut scope = ui::WakeScope::new();
        let waking = Arc::clone(wake);
        if let Err(error) = scope.bind(move |_| waking()) {
            tracing::warn!(%error, "the conversations are not woken");
        }
        Self {
            records,
            personal: RefCell::default(),
            wake: scope,
            open: RefCell::default(),
            asked: RefCell::default(),
        }
    }

    /// The member at this device: whose changes what the service records are theirs, and the
    /// author a message or a record of work is written by when the caller names nobody else.
    /// Nobody while no workspace is open.
    #[must_use]
    pub fn member(&self) -> MemberId {
        let open = self.open.borrow();
        open.values().next().map_or_else(
            || MemberId::person(""),
            |opened| opened.records.author().clone(),
        )
    }

    pub(crate) fn use_store(&self, store: crate::PersonResources) {
        self.personal.replace(Some(store));
    }

    /// Open the person's previous member record for recovery if it is already held.
    fn previous_learned(opened: &Opened, person: &str) -> Option<Vec<u8>> {
        use ui::resource::{DocumentCheckout, ResourceType};

        if opened.records.author().person_id() != Some(person) {
            return None;
        }
        let id = DocumentId::of_member(person, Learned::KIND)?;
        let checkout = DocumentCheckout::new(id, ResourceType::new(Learned::KIND));
        let store = opened.records.store_of(CHAT_OWNER).ok()?;
        let mut previous = match store.retained_document(&checkout) {
            Ok(document) => document?,
            Err(error) => {
                tracing::warn!(%error, "the previous learned facts could not be read");
                return None;
            }
        };
        Some(previous.save())
    }

    /// The workspace's agents record `opened` holds, opened here where it is not. `None` where the
    /// workspace keeps none — reading an app's record creates none, as [`Self::open_chat`] reads
    /// Chat's.
    fn workspace_agents(
        opened: &Opened,
    ) -> Option<WorkspaceResource<tilo_app::tilo_workspace_agent::WorkspaceAgents>> {
        use tilo_app::tilo_workspace_agent::{KIND as AGENTS_KIND, OWNER as AGENTS_OWNER};
        let id = collab_types::workspace_document_id(AGENTS_KIND);
        let held = opened
            .records
            .holds_resource(AGENTS_OWNER, &id, AGENTS_KIND)
            .unwrap_or(false);
        if !held {
            return None;
        }
        let access = tilo_action_policy::MemberAccess::new(
            opened.policy.clone(),
            tilo_action_policy::Acting::member(opened.records.author().clone()),
        );
        let permissions = PermissionContext::from_access(access)
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        WorkspaceResource::<tilo_app::tilo_workspace_agent::WorkspaceAgents>::open(
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
    /// record is the one Chat's window writes: no second writer, and no second verb.
    ///
    /// # Errors
    /// No open workspace holds the message `arguments` names, or Chat refused the record.
    fn record_work(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::ApiError> {
        let unknown = || {
            tilo_actions::ApiError::Unknown(tilo_actions::ActionAddress::new(resource, RECORD_WORK))
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
            tilo_actions::ApiError::InvalidArguments(
                "a record of work says how the work is going".to_owned(),
            )
        })?;
        let work: chat::chat::WorkUpdate = serde_json::from_value(work).map_err(|error| {
            tilo_actions::ApiError::InvalidArguments(format!("the work does not read: {error}"))
        })?;
        let say = arguments
            .get("say")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let summons = Id::from_raw(message);
        let invoke = chat::parts::ChatMessages::RECORD_WORK.with(
            chat::messages::chat_messages::RecordWork {
                message: summons.clone(),
                work,
                say,
            },
        );
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
                .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()));
        }
        Err(unknown())
    }

    /// Keep, correct or forget a fact in the person's own record of what their agents learned
    /// about them, as `invoker` (the agent answering a turn for them): `action` is the record's
    /// verb and `arguments` are its own. The record is the person's at this device, in the
    /// workspace that holds the turn's message; the first fact makes it. The record's verbs
    /// decide what a fact is, and the rule for a member's own record decides who may write it.
    ///
    /// # Errors
    /// The arguments do not read, no open workspace holds the turn's message, the record cannot
    /// be opened, or it refused the change.
    fn learn(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::ApiError> {
        use tilo_app::tilo_workspace_agent::learned::learned;
        let unknown =
            || tilo_actions::ApiError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        let invalid = |what: &str| tilo_actions::ApiError::InvalidArguments(what.to_owned());
        let text = |name: &str| arguments.get(name).and_then(serde_json::Value::as_str);
        let said = |name: &str, what: &str| {
            text(name)
                .and_then(tilo_resource::NonBlank::parse)
                .ok_or_else(|| invalid(what))
        };
        let fact = || {
            text("fact")
                .filter(|fact| !fact.is_empty())
                .map(Id::from_raw)
                .ok_or_else(|| invalid("a fact is named by its id"))
        };
        let invoke = match action {
            "learn" => Learned::LEARN.with(learned::Learn {
                text: said("text", "a fact says something")?,
                source: said("source", "a fact says where it came from")?,
            }),
            "correct" => Learned::CORRECT.with(learned::Correct {
                fact: fact()?,
                text: said("text", "a fact says something")?,
                source: text("source").map(str::to_owned),
            }),
            "forget" => Learned::FORGET.with(learned::Forget { fact: fact()? }),
            _ => return Err(unknown()),
        };
        let message = message_of(resource).map(Id::from_raw).ok_or_else(unknown)?;
        let member = self.member();
        let person = member.person_id().ok_or_else(unknown)?;
        let agent = invoker.acting.agent_id().ok_or_else(unknown)?;
        let personal = self.personal.borrow();
        let personal = personal
            .as_ref()
            .ok_or_else(|| tilo_actions::ApiError::Unavailable {
                code: "learned_unavailable".to_owned(),
                reason: "the person's workspace is unavailable on this device".to_owned(),
            })?;
        let open = self.open.borrow();
        for (workspace, opened) in open.iter() {
            let Some((_, part)) = &opened.part else {
                continue;
            };
            let Some(chat) = &opened.chat else {
                continue;
            };
            let messages = part.get();
            let Some(asked) = messages.messages.get(&message) else {
                continue;
            };
            // An agent of the workspace, in a turn the person asked in their own conversation
            // with it: what it learns there is theirs.
            let works_here = Self::workspace_agents(opened)
                .is_some_and(|agents| agents.get().active().any(|entry| entry.id.0 == agent));
            if asked.author != member
                || !works_here
                || !private_turn(&chat.get(), &asked.channel, person, agent)
            {
                return Err(unknown());
            }
            let id = personal_learned::record_id(workspace);
            let previous = Self::previous_learned(opened, person);
            return personal_learned::change(personal, &id, agent, previous.as_deref(), invoke)
                .map_err(tilo_actions::ApiError::Failed);
        }
        Err(unknown())
    }

    /// Keep a conversation's summary through Chat's own `keep-summary` on the workspace's open Chat
    /// record, as `invoker` (the agent answering a turn there): `arguments` are the verb's. Chat's
    /// verb decides who may keep one and how far it reaches; nothing is decided here.
    ///
    /// # Errors
    /// The arguments do not read, no open workspace holds the conversation, or Chat refused it.
    fn keep_summary(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::ApiError> {
        let text = |name: &str| {
            arguments
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    tilo_actions::ApiError::InvalidArguments(format!("a summary names its {name}"))
                })
        };
        let channel = Id::from_raw(text("conversation")?);
        let through_ts = arguments
            .get("through_ts")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
                tilo_actions::ApiError::InvalidArguments(
                    "a summary says when its last message was written".to_owned(),
                )
            })?;
        let invoke = chat::Chat::KEEP_SUMMARY.with(chat::chat::chat::KeepSummary {
            conversation: channel.clone(),
            through: text("through")?,
            through_ts,
            text: text("text")?,
        });
        let open = self.open.borrow();
        for opened in open.values() {
            let (Some(record), Some((_, part))) = (&opened.chat, &opened.part) else {
                continue;
            };
            if !part
                .get()
                .messages
                .iter()
                .any(|(_, message)| message.channel == channel)
            {
                continue;
            }
            return record
                .invoke_as(invoker, &invoke)
                .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()));
        }
        Err(tilo_actions::ApiError::Unknown(
            tilo_actions::ActionAddress::new(resource, KEEP_SUMMARY),
        ))
    }

    /// Open each workspace's conversations as `person`, where they are not open yet, and follow
    /// them from then on.
    pub(crate) fn feed(&self, person: &str, workspaces: Vec<WorkspaceFacts>) {
        for facts in &workspaces {
            self.open_chat(facts, person);
        }
    }

    /// Open the workspace's chat record and the head of its messages here where they are not.
    /// A workspace whose person never opened Chat holds no record, and the reader creates none.
    fn open_chat(&self, workspace: &WorkspaceFacts, person: &str) {
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
            members: chat::projection::PeopleNames::new(),
            chat: None,
            part: None,
        });
        opened.members.clone_from(&workspace.members);
        let permissions = PermissionContext::from_access(tilo_action_policy::MemberAccess::person(
            workspace.policy.clone(),
            person.to_owned(),
        ))
        .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        let chat_id = DocumentId::package_primary();
        if opened.chat.is_none() {
            let held = opened
                .records
                .holds_resource(CHAT_OWNER, chat_id.as_str(), chat::Chat::KIND)
                .unwrap_or(false);
            if !held {
                return;
            }
            opened.chat = WorkspaceResource::<chat::Chat>::open_primary(
                &opened.records,
                CHAT_OWNER,
                CHAT_APP,
                Some(LivePermissions::fixed(permissions.clone())),
                Some(self.wake.handle()),
            );
        }
        if opened.chat.is_none() {
            return;
        }
        let Some(head) = opened
            .records
            .store_of(CHAT_OWNER)
            .ok()
            .and_then(|store| store.held_parts(&chat::parts::chain()).into_iter().max())
        else {
            return;
        };
        if opened.part.as_ref().map(|(held, _)| *held) != Some(head) {
            let part = WorkspaceResource::<chat::parts::ChatMessages>::open_at(
                &opened.records,
                CHAT_OWNER,
                chat::parts::part(head).as_str(),
                CHAT_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
            opened.part = part.map(|part| (head, part));
        }
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
                Some(chat.display_names(
                    &Id::from_raw(channel),
                    Some(opened.records.author()),
                    &opened.members,
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
            let held = chat::parts::ChatMessages::clone(part.get().as_ref());
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
            let me = opened.records.author();
            let names = chat::views::Names {
                me: crate::platform::person::person_name(
                    ui::local_identity::load().map(|identity| identity.display_name),
                    me.id(),
                ),
                members: opened.members.clone(),
            };
            return Some(chat::views::Channel::read(
                &chat, &rows, channel, me, &names,
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

/// A turn asked from the canvas, as a declared type: Chat's own kind for a message's item.
pub struct TurnRow;

impl tilo_resource::words::RowType for TurnRow {
    const KIND: &'static str = chat::views::TURN_KIND;
    const LABEL: &'static str = "Answer";

    /// Chat's own words for a message's item, as its window publishes it.
    fn words() -> tilo_actions::ResourceType {
        tilo_actions::ResourceType {
            noun: tilo_actions::Noun::new("answer", "answers"),
            app: tilo_actions::Noun::new("Chat", "Chat"),
            root: false,
        }
    }

    fn effects(id: &tilo_resource::Id) -> Vec<tilo_actions::ActionSpec> {
        <TurnEffect as tilo_resource::Effect>::row_specs(Self::KIND, id)
    }
}

/// Chat's message item, as a declared type: Chat's own kind, through which the session publishes
/// a turn's summons with Chat's window closed.
pub struct ChatMessage;

impl tilo_resource::words::RowType for ChatMessage {
    const KIND: &'static str = "chat.messages";
    const LABEL: &'static str = "Message";
}

/// The whole conversation a turn was asked in, as a declared type: Chat's own channel.
pub struct ConversationTurn;

impl tilo_resource::words::RowType for ConversationTurn {
    const KIND: &'static str = "chat.workspace.channels";
    const LABEL: &'static str = "Conversation";
}

/// What a turn offers on its place's handle, declared once: the whole conversation it was asked
/// in, opened beside it (canvas.md §7, "the conversation is reached from the handle").
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.chat.turn")]
pub enum TurnEffect {
    /// Whole conversation
    ///
    /// Opens the whole conversation this answer was asked in, beside it.
    #[effect(on = TurnRow, kind = "undoable", label = "session-turn-conversation")]
    Conversation,
}

/// One turn as a place's resource (canvas.md §3): the message `question` a person asked an agent
/// in, named by what they asked, drawn through Chat's `turn` view.
fn turn_resource(question: &str, asked: &str) -> tilo_actions::ResourceSpec {
    let label = asked.lines().next().unwrap_or_default().trim();
    let mut spec = tilo_resource::LiveRow::typed::<TurnRow>(turn_address(question), label)
        .view(tilo_actions::ResourceViewSpec::new(
            tilo_session_core::layout::TURN_VIEW,
            label.to_owned(),
            "open",
        ))
        .into_resource()
        // Chat's own words for a message's item, as its window publishes it.
        .with_type(tilo_actions::ResourceType {
            noun: tilo_actions::Noun::new("answer", "answers"),
            app: tilo_actions::Noun::new("Chat", "Chat"),
            root: false,
        });
    spec.app_id = Some("chat".to_owned());
    spec
}

/// Where a conversation is published: Chat's channel item at its stable address
/// (`chat.workspace.channels:<id>`), a direct conversation's included.
const CHANNELS: &str = "chat.workspace.channels:";

/// The conversation `channel` as a place's resource: Chat's channel, through Chat's own page,
/// with Chat's own `send` its composer runs. What a turn's "Whole conversation" places beside it.
fn conversation_resource(channel: &str, label: String) -> tilo_actions::ResourceSpec {
    let mut spec = tilo_resource::LiveRow::typed::<ConversationTurn>(
        format!("{CHANNELS}{channel}"),
        label.clone(),
    )
    .view(tilo_actions::ResourceViewSpec::new("page", label, "open"))
    .into_resource();
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
/// workspace's open Chat record, the one Chat's window writes — never a second writer on the
/// store.
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
            // (`chat::chat::reply_id`) before either exists — so the summons's item carries
            // the verb from the moment the turn does, never only once Chat's window publishes
            // the message. The item is Chat's own, published through `ChatMessages`'s declaration,
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
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        let invoker = tilo_actions::ActionInvoker::member(self.conversations.member());
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
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
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
            let asked = if invoker.acting.by.id().is_empty() {
                self.conversations.member().into()
            } else {
                invoker.acting.by.clone()
            };
            self.layout
                .borrow_mut()
                .open_conversation_of(&asked, resource)
                .map_err(|why| tilo_actions::ApiError::Unavailable {
                    code: "nowhere_to_open".to_owned(),
                    reason: why,
                })?;
            return Ok(tilo_service::Started::Done(
                tilo_actions::ActionOutput::default(),
            ));
        }
        if action == KEEP_SUMMARY {
            return self
                .conversations
                .keep_summary(invoker, resource, arguments)
                .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()));
        }
        if LEARNED_VERBS.contains(&action) && message_of(resource).is_some() {
            return self
                .conversations
                .learn(invoker, resource, action, arguments)
                .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()));
        }
        if action != RECORD_WORK {
            return Err(tilo_actions::ApiError::Unknown(
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

/// Every summons whose reply has no item Chat's window publishes yet: the person's messages an
/// agent in the conversation has not answered, and that no held message of the part answers. The
/// reply each names is the one its id and the agent's derive (`chat::chat::reply_id`), so this
/// returns the summonses — the item the agent's verb is offered on — never the reply.
/// The summonses whose turn has no reply yet: the person's messages in a conversation an agent
/// takes part in, which no held message answers. The item each is published as carries Chat's
/// `record-work`, its message bound, so the agent's verb is offered on the address it answers from
/// before the reply the verb makes exists — the reply's own item is Chat's window's once it does.
/// The summons whose turn is going: the person's messages in a conversation an agent takes part
/// in, whose reply the agent has not finished — absent yet, or still working (no state that ends
/// it) — or whose reply the person asked again (`chat`'s `retry`, which gives the reply up). The
/// same rule the agent engine reads turns by, so the item that records the work is offered exactly
/// while the agent answers there: from the moment the turn exists, through every record it makes
/// on the reply it is writing, and again when a failed reply was retried.
fn summonses(chat: &chat::Chat, part: &chat::parts::ChatMessages) -> Vec<Id> {
    let mut out: Vec<Id> = Vec::new();
    for (id, message) in part.messages.iter() {
        if message.text.trim().is_empty() || chat::summons::asks_for(message).is_none() {
            continue;
        }
        if chat
            .channels
            .get(&message.channel)
            .is_some_and(|conversation| conversation.archived)
        {
            continue;
        }
        let agents: Vec<MemberId> = chat
            .conversation_members(&message.channel)
            .into_iter()
            .filter(|member| !member.is_person())
            .collect();
        for agent in agents {
            // The reply of the attempt the message is in: a retry moved it on, so the reply that
            // failed stands as history and the item offers the next attempt's address.
            let reply = chat::chat::reply_id_at(id, &agent, message.attempts);
            let going = part
                .messages
                .get(&reply)
                // A reply without work is a plain answer; one that keeps work is going until
                // its work ends.
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
/// own `record-work` — the declaration `ChatMessages::RECORD_WORK` publishes, whose action type is
/// Chat's, unbound so the agent names the message it records the work of as it does on Chat's
/// window's item. Built by [`tilo_resource::declaration_spec`], so the action an agent invokes is
/// Chat's verb, not a copy of it.
fn summons_item(id: &Id) -> tilo_actions::ResourceSpec {
    let kind = chat::parts::ChatMessages::KIND;
    let mut spec =
        tilo_resource::typed_resource::<ChatMessage>(format!("{kind}/messages/{id}"), id.as_str())
            // Chat's own words for a message, as its window publishes it.
            .with_type(tilo_actions::ResourceType {
                noun: tilo_actions::Noun::new("message", "messages"),
                app: tilo_actions::Noun::new("Chat", "Chat"),
                root: false,
            });
    spec.app_id = Some("chat".to_owned());
    let declaration = tilo_resource::vocabulary::<chat::parts::ChatMessages>()
        .declarations()
        .iter()
        .find(|declaration| declaration.id == RECORD_WORK)
        .cloned();
    if let Some(declaration) = declaration {
        spec = spec.with_action(tilo_resource::declaration_spec(&declaration));
    }
    // The summary of the conversation the turn is in is kept from the turn's own item too: the
    // agent keeps it while it answers, whether or not Chat's window is open to publish the verb.
    let summary = tilo_resource::vocabulary::<chat::Chat>()
        .declarations()
        .iter()
        .find(|declaration| declaration.id == KEEP_SUMMARY)
        .cloned();
    if let Some(declaration) = summary {
        spec = spec.with_action(tilo_resource::declaration_spec(&declaration));
    }
    // What the agent learns about its person while it answers is kept from the turn's item as
    // well: the person's own record, written through its own verbs, with Chat's window closed.
    for declaration in
        tilo_resource::vocabulary::<tilo_app::tilo_workspace_agent::learned::Learned>()
            .declarations()
            .iter()
            .filter(|declaration| LEARNED_VERBS.contains(&declaration.id))
    {
        spec = spec.with_action(tilo_resource::declaration_spec(declaration));
    }
    spec
}

/// Learned facts belong to exchanges between the person and their agent.
fn private_turn(chat: &chat::Chat, channel: &Id, person: &str, agent: &str) -> bool {
    let members = chat.conversation_members(channel);
    members.len() == 2
        && members.contains(&MemberId::person(person))
        && members.contains(&MemberId::agent(agent))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A turn's item offers what its agent writes while it answers, with Chat's window closed:
    /// the reply's work, the conversation's summary, and the facts it learns about its person.
    #[test]
    fn a_turns_item_offers_the_reply_the_summary_and_what_was_learned() {
        let item = summons_item(&Id::from_raw("m1"));
        let offered: Vec<&str> = item
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for verb in ["record-work", "keep-summary", "learn", "correct", "forget"] {
            assert!(offered.contains(&verb), "{verb}: {offered:?}");
        }
    }

    /// The agent the tests' conversations take in.
    const AGENT: &str = "tide";
    /// The person sitting here, by their member key.
    const ME: &str = "me";
    /// The conversation the two of them have.
    const CONVERSATION: &str = "c_tide";

    /// A workspace holding one conversation between `ME` and `AGENT` .
    fn workspace() -> chat::Chat {
        let mut chat = chat::Chat::default();
        chat.channels.push(
            Id::from_raw(CONVERSATION),
            chat::chat::Conversation {
                name: "Tide".into(),
                members: vec![MemberId::person(ME), MemberId::agent(AGENT)],
                ..chat::chat::Conversation::default()
            },
        );
        chat
    }

    /// A message `author` wrote in `CONVERSATION` at `ts` .
    fn message(author: MemberId, text: &str, ts: i64) -> chat::chat::Message {
        chat::chat::Message::new(Id::from_raw(CONVERSATION), author, ts, text.into())
    }

    /// The head part of the messages, holding `messages` .
    fn part(messages: Vec<(&str, chat::chat::Message)>) -> chat::parts::ChatMessages {
        let mut part = chat::parts::ChatMessages::default();
        for (id, message) in messages {
            part.messages.push(Id::from_raw(id), message);
        }
        part
    }

    /// A summons's item is offered while its turn goes: a message a cause wrote for the person it
    /// names, as one they wrote; not once its agent's reply failed; and again when the person
    /// asked it once more.
    #[test]
    fn a_summons_is_offered_while_its_turn_goes() {
        let chat = workspace();
        let cause = chat::cause::Cause {
            kind: tilo_resource::Atom("reminder".into()),
            label: "Call mum".into(),
            key: tilo_resource::Atom("run-1".into()),
            person: MemberId::person(ME),
        };
        let id = cause.message();
        let mut reminded = message(MemberId::bot("b_events"), "Call mum about Sunday", 10);
        reminded.cause = Some(cause);
        let head = part(vec![
            (id.as_str(), reminded),
            (
                "m_bridge",
                message(MemberId::bot("b_bridge"), "do as I say", 20),
            ),
            ("m_own", message(MemberId::agent(AGENT), "on it", 30)),
        ]);
        assert_eq!(summonses(&chat, &head), vec![id]);

        let mut asked = message(MemberId::person(ME), "add the revenue curve", 10);
        let mut head = part(vec![("m1", asked.clone())]);
        let mut failed = message(MemberId::agent(AGENT), "", 20);
        failed.parent = Some(Id::from_raw("m1"));
        failed.work = Some(chat::chat::Work {
            state: Some(chat::chat::Transition {
                state: chat::model::AgentWorkState::ProviderFinished {
                    outcome: chat::agent_stream::AgentFinish::Failed,
                },
                since: 20,
            }),
            ..chat::chat::Work::default()
        });
        head.messages.push(
            chat::chat::reply_id(&Id::from_raw("m1"), &MemberId::agent(AGENT)),
            failed,
        );
        assert_eq!(
            summonses(&chat, &head),
            Vec::<Id>::new(),
            "a failed reply ends the turn until the person asks again"
        );
        asked.attempts = 1;
        head.messages.push(Id::from_raw("m1"), asked);
        assert_eq!(
            summonses(&chat, &head),
            vec![Id::from_raw("m1")],
            "the summons's item offers `record-work` again, so the agent's new answer lands"
        );
    }
}
