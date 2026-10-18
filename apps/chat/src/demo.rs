//! The demo room a screenshot or a test shows: its people, channels and
//! conversation, read by `me`. Never a real record's content.
//!
//! The chat record ([`demo`]) holds the people and rooms; the conversation
//! is the first part of the chat's messages ([`demo_part`]). A tool or a
//! test without a window holds both as documents ([`Room`]).

use automerge::AutoCommit;
use ui::tilo_actions::DoorError;
use ui::tilo_resource::{Atom, Edit, Id, Invoke, ModelError, Replica, Set};

use crate::messages::{Messages, chat_part};
use crate::model::{Attachment, ChannelKind, EditRecord, Presence};
use crate::parts::ChatPart;
use crate::record::{Category, Channel, Chat, Message, Person, Revision, chat};

/// Deterministic "now" for the demo so a screenshot is stable: 2026-06-30
/// 10:40 UTC, just before the capture clock's 10:42.
pub const NOW: i64 = 1_782_470_400;

/// One row of the demo channel roster: (id, category, name, topic, kind,
/// sort order within its category).
type DemoChannelSeed = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    ChannelKind,
    u32,
);

/// The emoji the catalogue calls `name` ("thumbs up"), as a reaction or a
/// message holds it.
fn named(name: &str) -> String {
    emoji::Catalogue::english()
        .emoji()
        .iter()
        .find(|emoji| emoji.name() == name)
        .map_or_else(String::new, |emoji| emoji.text().to_owned())
}

/// A demo message.
#[derive(Clone, Debug, Default)]
struct NewMessage {
    author: String,
    ts: i64,
    text: String,
    parent: Option<String>,
    reactions: Vec<(String, Vec<String>)>,
    attachment: Option<Attachment>,
    history: Vec<EditRecord>,
    edited: bool,
}

struct Demo;

impl Demo {
    fn channels_acme() -> &'static [DemoChannelSeed] {
        &[
            (
                "c_general",
                "cat_acme_main",
                "general",
                "Company-wide announcements",
                ChannelKind::Text,
                0,
            ),
            (
                "c_crit",
                "cat_acme_main",
                "design-crit",
                "Share work, get sharp feedback",
                ChannelKind::Text,
                1,
            ),
            (
                "c_frontend",
                "cat_acme_main",
                "frontend",
                "ui toolkit + app shells",
                ChannelKind::Text,
                2,
            ),
            (
                "c_random",
                "cat_acme_main",
                "random",
                "Off-topic & watercooler",
                ChannelKind::Text,
                3,
            ),
            (
                "c_standup",
                "cat_acme_voice",
                "Daily Standup",
                "",
                ChannelKind::Voice,
                0,
            ),
            (
                "c_huddle",
                "cat_acme_voice",
                "Design Huddle",
                "",
                ChannelKind::Voice,
                1,
            ),
        ]
    }

    fn channels_tilo_and_jam() -> &'static [DemoChannelSeed] {
        &[
            (
                "c_kernel",
                "cat_tilo_eng",
                "kernel",
                "bootc + immutable base",
                ChannelKind::Text,
                0,
            ),
            (
                "c_compositor",
                "cat_tilo_eng",
                "compositor",
                "2D clusters & sandboxing",
                ChannelKind::Text,
                1,
            ),
            (
                "c_fssync",
                "cat_tilo_eng",
                "fs-sync",
                "iroh projection & hydration",
                ChannelKind::Text,
                2,
            ),
            (
                "c_releases",
                "cat_tilo_eng",
                "releases",
                "ship logs",
                ChannelKind::Text,
                3,
            ),
            (
                "c_warroom",
                "cat_acme_voice",
                "War Room",
                "",
                ChannelKind::Voice,
                0,
            ),
            (
                "c_ideas",
                "cat_jam_main",
                "ideas",
                "weekend hacks",
                ChannelKind::Text,
                0,
            ),
            (
                "c_music",
                "cat_jam_main",
                "music",
                "now playing",
                ChannelKind::Text,
                1,
            ),
        ]
    }

    fn design_crit_early() -> Vec<(&'static str, NewMessage)> {
        let m = |off_min: i64| NOW - off_min * 60;
        vec![
            (
                "m_open",
                NewMessage {
                    author: "u_aria".into(),
                    ts: m(96),
                    text: "Morning! Pushed the new call overlay mock — pulling tiles into a 2x2 grid reads way calmer than the old filmstrip.".into(),
                    reactions: vec![(named("party popper"), vec!["u_me".into(), "u_milo".into()])],
                    ..Default::default()
                },
            ),
            (
                "m_concept",
                NewMessage {
                    author: "u_aria".into(),
                    ts: m(95),
                    text: "Here's the frame — the host tile keeps the live camera, everyone else collapses to chips when they're audio-only.".into(),
                    attachment: Some(Attachment::Image {
                        name: "call-overlay-v3.png".into(),
                        blob: crate::images::DEMO_CALL_GRID.into(),
                        w: 640,
                        h: 360,
                    }),
                    reactions: vec![
                        (named("red heart"), vec!["u_me".into(), "u_sora".into(), "u_milo".into()]),
                        (named("fire"), vec!["u_devon".into()]),
                    ],
                    ..Default::default()
                },
            ),
            (
                "m_me1",
                NewMessage {
                    author: "u_me".into(),
                    ts: m(91),
                    text: "Love it. One ask: can the 'follow the host' affordance live on the host tile itself instead of the controls bar?".into(),
                    ..Default::default()
                },
            ),
            (
                "m_milo",
                NewMessage {
                    author: "u_milo".into(),
                    ts: m(84),
                    text: "+1 — and we should read every colour from the OS theme so it matches whatever the cluster is themed to.".into(),
                    reactions: vec![(named("thumbs up"), vec!["u_me".into(), "u_aria".into()])],
                    ..Default::default()
                },
            ),
        ]
    }

    fn design_crit_late() -> Vec<(&'static str, NewMessage)> {
        let m = |off_min: i64| NOW - off_min * 60;
        vec![
            (
                "m_devon",
                NewMessage {
                    author: "u_devon".into(),
                    ts: m(70),
                    text: "Dropping the transport notes here so they don't get lost.".into(),
                    attachment: Some(Attachment::File {
                        name: "moq-over-iroh.md".into(),
                        blob: crate::images::DEMO_NOTES.into(),
                        size: 18_944,
                    }),
                    ..Default::default()
                },
            ),
            (
                "m_clip",
                NewMessage {
                    author: "u_sora".into(),
                    ts: m(64),
                    text: "Screen recording of the tile-promotion motion — this is the feel I'm after:".into(),
                    attachment: Some(Attachment::Video {
                        name: "tile-promotion.webm".into(),
                        blob: crate::images::DEMO_CLIP.into(),
                        w: 1280,
                        h: 720,
                        size: 6_815_744,
                    }),
                    reactions: vec![(named("fire"), vec!["u_me".into(), "u_aria".into()])],
                    ..Default::default()
                },
            ),
            (
                "m_sora",
                NewMessage {
                    author: "u_sora".into(),
                    ts: m(41),
                    text: "Wired the screen-share stage — whoever presents gets a big featured tile and everyone else drops to the filmstrip beneath it. Reads really cleanly.".into(),
                    edited: true,
                    history: vec![
                        EditRecord {
                            ts: m(43),
                            text: "Wired up screen-share — the presenter gets a featured tile, everyone else goes to a filmstrip.".into(),
                        },
                    ],
                    reactions: vec![
                        (named("rocket"), vec!["u_me".into(), "u_aria".into(), "u_milo".into(), "u_devon".into()]),
                        (named("eyes"), vec!["u_iris".into()]),
                    ],
                    ..Default::default()
                },
            ),
            (
                "m_me2",
                NewMessage {
                    author: "u_me".into(),
                    ts: m(38),
                    text: "That's the one. Starting a huddle to walk through the grid — jump in.".into(),
                    ..Default::default()
                },
            ),
        ]
    }

    fn design_crit_thread(parent_id: &str) -> Vec<NewMessage> {
        let m = |off_min: i64| NOW - off_min * 60;
        let thread: Vec<NewMessage> = vec![
            NewMessage {
                author: "u_milo".into(),
                ts: m(93),
                text: "Should the chips show a mic-level ring when someone's talking?".into(),
                parent: Some(parent_id.into()),
                ..Default::default()
            },
            NewMessage {
                author: "u_aria".into(),
                ts: m(92),
                text: "Yeah — a thin accent ring that pulses. I'll spec the motion token.".into(),
                parent: Some(parent_id.into()),
                reactions: vec![(named("thumbs up"), vec!["u_me".into()])],
                ..Default::default()
            },
            NewMessage {
                author: "u_me".into(),
                ts: m(90),
                text: "Perfect, that keeps the audio-only state legible.".into(),
                parent: Some(parent_id.into()),
                ..Default::default()
            },
        ];
        thread
    }

    fn general_welcome() -> NewMessage {
        let m = |off_min: i64| NOW - off_min * 60;
        NewMessage {
            author: "u_iris".into(),
            ts: m(220),
            text: format!(
                "Welcome to Acme Design {} Say hello and tell us what you're working on this week.",
                named("waving hand")
            ),
            reactions: vec![(
                named("waving hand"),
                vec!["u_me".into(), "u_aria".into(), "u_milo".into()],
            )],
            ..Default::default()
        }
    }
}

/// The demo room, with `me` as the person reading it: its people and
/// channels. Its conversation is [`demo_part`].
#[must_use]
pub fn demo(me: &str) -> Chat {
    let mut chat = Chat::default();
    for (id, name, initials, color, status) in [
        (me, crate::identity::DEMO_NAME, "QD", 0u32, Presence::Online),
        ("u_aria", "Aria Chen", "AC", 1, Presence::Online),
        ("u_milo", "Milo Park", "MP", 2, Presence::Idle),
        ("u_sora", "Sora Vega", "SV", 3, Presence::Online),
        ("u_devon", "Devon Reyes", "DR", 5, Presence::Dnd),
        ("u_iris", "Iris Wolfe", "IW", 6, Presence::Offline),
    ] {
        chat.people.push(Person {
            id: Atom(id.to_owned()),
            name: name.to_owned(),
            initials: initials.to_owned(),
            color,
            status,
            agent: false,
        });
    }
    for (id, name) in [
        ("cat_acme_main", "Channels"),
        ("cat_acme_voice", "Voice"),
        ("cat_tilo_eng", "Engineering"),
        ("cat_jam_main", "Hangout"),
    ] {
        chat.categories.push(
            Id::from_raw(id),
            Category {
                name: name.to_owned(),
            },
        );
    }
    for (id, category, name, topic, kind, _) in Demo::channels_acme()
        .iter()
        .chain(Demo::channels_tilo_and_jam())
    {
        chat.open_channel(
            Id::from_raw(*id),
            Channel {
                category: Some(Id::from_raw(*category)),
                name: (*name).to_owned(),
                topic: (*topic).to_owned(),
                kind: *kind,
                archived: false,
                conversation_with: None,
            },
        );
    }
    chat
}

/// The demo conversation, as the first part of the chat's messages, with
/// `me` as the person reading it.
#[must_use]
pub fn demo_part(me: &str) -> ChatPart {
    let participants = demo(me).participants();
    let mut part = ChatPart::default();
    let mine = |id: &str| {
        if id == crate::model::ME {
            me.to_owned()
        } else {
            id.to_owned()
        }
    };
    let mut post = |id: String, channel: &str, message: NewMessage| {
        let mut reactions = Set::new();
        for (emoji, users) in message.reactions {
            for user in users {
                reactions.insert(crate::record::Reacted {
                    emoji: emoji.clone(),
                    user: mine(&user),
                });
            }
        }
        part.messages.push(
            Id::from_raw(id),
            Message {
                channel: Id::from_raw(channel),
                author: Atom(mine(&message.author)),
                ts: message.ts,
                mentions: crate::mention::resolve(&message.text, &participants),
                text: message.text,
                edited: message.edited,
                history: message
                    .history
                    .into_iter()
                    .map(|record| Revision {
                        ts: record.ts,
                        text: record.text,
                    })
                    .collect(),
                parent: message.parent.map(Id::from_raw),
                reactions,
                attachment: message.attachment,
                ..Message::default()
            },
        );
    };
    let mut crit = Demo::design_crit_early();
    crit.extend(Demo::design_crit_late());
    for (id, message) in crit {
        post(id.to_owned(), "c_crit", message);
    }
    for (index, message) in Demo::design_crit_thread("m_concept")
        .into_iter()
        .enumerate()
    {
        post(format!("m_thread_{index}"), "c_crit", message);
    }
    post("m_welcome".to_owned(), "c_general", Demo::general_welcome());
    part
}

/// `part` written into an empty part, message by message: what a capture's
/// scenario and a test start from.
#[must_use]
pub fn part_edits(part: &ChatPart) -> Vec<Edit<ChatPart>> {
    let paths = ChatPart::paths();
    part.messages
        .iter()
        .enumerate()
        .map(|(at, (id, item))| paths.messages.insert_as(id, at, item))
        .collect()
}

/// `chat` written into an empty record, item by item: what a capture's
/// scenario and a test start from.
#[must_use]
pub fn edits(chat: &Chat) -> Vec<Edit<Chat>> {
    let paths = Chat::paths();
    let mut edits = Vec::new();
    for (at, (id, item)) in chat.categories.iter().enumerate() {
        edits.push(paths.categories.insert_as(id, at, item));
    }
    for (at, (id, item)) in chat.channels.iter().enumerate() {
        edits.push(paths.channels.insert_as(id, at, item));
    }
    for person in &chat.people {
        edits.push(paths.people.insert(person));
    }
    for (at, (id, report)) in chat.feedback.iter().enumerate() {
        edits.push(paths.feedback.insert_as(id, at, report));
    }
    if !chat.agent_names.is_empty() {
        edits.push(paths.agent_names.set(chat.agent_names.clone()));
    }
    if !chat.read.is_empty() {
        edits.push(paths.read.set(chat.read.clone()));
    }
    edits
}

/// Write `chat` into `doc`, a document holding no chat yet, as one change.
///
/// # Errors
/// When `chat` does not fit the record's model.
pub fn write(doc: &mut AutoCommit, chat: &Chat) -> Result<(), ModelError> {
    initialized(doc)?;
    for edit in edits(chat) {
        edit.apply_local(doc)?;
    }
    commit_genesis(doc);
    Ok(())
}

/// Write `part` into `doc`, a document holding no part yet, as one change:
/// a part of the chat's messages as a tool or a test holds it.
///
/// # Errors
/// When `part` does not fit the part's model.
pub fn write_part(doc: &mut AutoCommit, part: &ChatPart) -> Result<(), ModelError> {
    <ChatPart as ui::tilo_resource::Resource>::initialize(doc)?;
    commit_genesis(doc);
    for edit in part_edits(part) {
        edit.apply_local(doc)?;
    }
    doc.commit();
    Ok(())
}

/// Commit what a brand-new chat starts with, named as its genesis.
fn commit_genesis(doc: &mut AutoCommit) {
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
}

/// `doc` with the record's genesis, when it holds no chat yet.
fn initialized(doc: &mut AutoCommit) -> Result<(), ModelError> {
    use automerge::ReadDoc as _;
    if doc.get(automerge::ROOT, "channels")?.is_none() {
        <Chat as ui::tilo_resource::Resource>::initialize(doc)?;
        commit_genesis(doc);
    }
    Ok(())
}

/// The demo room's chat record in `doc`, read by [`crate::model::ME`]: its
/// people and channels, without the conversation.
///
/// # Errors
/// When `doc` already holds a chat the demo does not fit into.
pub fn seed(doc: &mut AutoCommit) -> Result<(), ModelError> {
    write(doc, &demo(crate::model::ME))
}

/// The demo room in `room`, read by [`crate::model::ME`]: its chat record,
/// and its conversation as the first part of its messages.
///
/// # Errors
/// When `room` already holds a chat the demo does not fit into.
pub fn seed_room(room: &mut Room) -> Result<(), DoorError> {
    seed(&mut room.chat).map_err(|error| DoorError::Failed(error.to_string()))?;
    for edit in part_edits(&demo_part(crate::model::ME)) {
        let insert =
            Invoke::edit(&edit).map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
        room.apply_part_as(crate::model::ME, insert)?;
    }
    Ok(())
}

/// A chat as a tool or a test holds it without a window: the room's chat
/// record, as a document, and the parts of its messages, as the chain a
/// window holds them in ([`ui::framework::Chain`]). It reads and changes
/// like the chat record it dereferences to; a verb on the messages applies
/// through the chain to the part it changes ([`Room::apply_part_as`]).
pub struct Room {
    /// The room's chat record.
    pub chat: AutoCommit,
    /// The parts of its messages.
    parts: Option<ui::framework::Chain<ChatPart>>,
    /// The chat record as the chain's actions read it: refreshed before
    /// each of them.
    base: std::rc::Rc<std::cell::RefCell<std::rc::Rc<Chat>>>,
}

impl Default for Room {
    fn default() -> Self {
        let base: std::rc::Rc<std::cell::RefCell<std::rc::Rc<Chat>>> = std::rc::Rc::default();
        let read = std::rc::Rc::clone(&base);
        Self {
            chat: AutoCommit::new(),
            parts: ui::framework::Chain::detached(
                <crate::ChatShell as ui::framework::ResourceApp>::ID,
                crate::model::ME,
                move || std::rc::Rc::clone(&read.borrow()),
            ),
            base,
        }
    }
}

impl std::ops::Deref for Room {
    type Target = AutoCommit;

    fn deref(&self) -> &AutoCommit {
        &self.chat
    }
}

impl std::ops::DerefMut for Room {
    fn deref_mut(&mut self) -> &mut AutoCommit {
        &mut self.chat
    }
}

impl Room {
    /// A room holding the chat record `chat` and no messages yet.
    #[must_use]
    pub fn of(chat: AutoCommit) -> Self {
        Self {
            chat,
            ..Self::default()
        }
    }

    /// The demo room, read by [`crate::model::ME`].
    ///
    /// # Errors
    /// Never for an empty room: the demo fits it.
    pub fn demo() -> Result<Self, DoorError> {
        let mut room = Self::default();
        seed_room(&mut room)?;
        Ok(room)
    }

    /// The chain the messages are held in, for a window over this room.
    #[must_use]
    pub fn chain(&self) -> Option<ui::framework::Chain<ChatPart>> {
        self.parts.clone()
    }

    /// The chat record as the chain's actions read it: what a window over
    /// this room refreshes when it reads the record.
    #[must_use]
    pub fn base(&self) -> std::rc::Rc<std::cell::RefCell<std::rc::Rc<Chat>>> {
        std::rc::Rc::clone(&self.base)
    }

    /// The chat record as it stands.
    #[must_use]
    pub fn read(&self) -> Chat {
        crate::projection::read(&self.chat)
    }

    /// The parts of the messages, read.
    #[must_use]
    pub fn messages(&self) -> Messages {
        self.parts
            .as_ref()
            .map(|chain| Messages::new(chain.parts()))
            .unwrap_or_default()
    }

    /// The numbers of the parts held.
    #[must_use]
    pub fn part_numbers(&self) -> Vec<u64> {
        self.messages()
            .parts()
            .iter()
            .map(|(index, _)| *index)
            .collect()
    }

    /// Part `index` as a document's bytes, for a tool that carries it to
    /// another device.
    #[must_use]
    pub fn part_bytes(&self, index: u64) -> Option<Vec<u8>> {
        self.parts.as_ref()?.part_bytes(index)
    }

    /// The messages of `channel`, oldest first, as the window draws them.
    #[must_use]
    pub fn message_rows(&self, channel: &str) -> Vec<crate::model::Message> {
        self.read().message_rows(&self.messages(), channel)
    }

    /// The message `id`, in whichever channel and part it is.
    #[must_use]
    pub fn message_row(&self, id: &str) -> Option<crate::model::Message> {
        self.read().message_row(&self.messages(), id)
    }

    /// Apply `invoke`, a verb of the chat record, as `author` invokes it.
    ///
    /// # Errors
    /// As [`apply_as`].
    pub fn apply_as(&mut self, author: &str, invoke: Invoke<Chat>) -> Result<(), DoorError> {
        apply_as(&mut self.chat, author, invoke)
    }

    /// Apply `invoke`, a verb on the messages, as `author` invokes it: read
    /// against the chat record where it is invoked and applied through the
    /// chain to the part it changes (started when it is new). The part's
    /// number.
    ///
    /// # Errors
    /// The chat record's refusal, or the verb's own.
    pub fn apply_part_as(
        &mut self,
        author: &str,
        invoke: Invoke<ChatPart>,
    ) -> Result<u64, DoorError> {
        self.apply_part_at(author, ui::tilo_resource::origin_now_ms(), invoke)
    }

    /// [`Self::apply_part_as`] at `now_ms`, the action's time.
    ///
    /// # Errors
    /// As [`Self::apply_part_as`].
    pub fn apply_part_at(
        &mut self,
        author: &str,
        now_ms: i64,
        invoke: impl Into<Invoke<ChatPart>>,
    ) -> Result<u64, DoorError> {
        let invoke = invoke.into();
        let chain = self
            .parts
            .as_ref()
            .ok_or_else(|| DoorError::Failed("the room holds no messages".into()))?;
        if let Some(refusal) = invoke.refusal() {
            return Err(DoorError::InvalidArguments(refusal.to_owned()));
        }
        *self.base.borrow_mut() = std::rc::Rc::new(crate::projection::read(&self.chat));
        chain
            .stage_at(&invoke, author, now_ms)
            .map_err(|error| DoorError::Failed(error.to_string()))
    }

    /// Post `text` in `channel` as `author`; the new message's id.
    ///
    /// # Errors
    /// As [`Self::apply_part_as`].
    pub fn say(&mut self, author: &str, channel: &str, text: &str) -> Result<String, DoorError> {
        let before: Vec<String> = self
            .message_rows(channel)
            .into_iter()
            .map(|m| m.id)
            .collect();
        self.apply_part_as(author, post(channel, text, None))?;
        Ok(self
            .message_rows(channel)
            .into_iter()
            .map(|message| message.id)
            .find(|id| !before.contains(id))
            .unwrap_or_default())
    }
}

// What a tool or a test holding a bare chat document does with it: apply
// the record's verbs as a named person, the way the door applies them.

/// Apply `invoke` to `doc` through the door, as `author` invokes it.
///
/// # Errors
/// The door's refusal or the verb's own.
pub fn apply_as(doc: &mut AutoCommit, author: &str, invoke: Invoke<Chat>) -> Result<(), DoorError> {
    apply_at(doc, author, 0, invoke)
}

/// [`apply_as`] at `now_ms`, the action's time.
///
/// # Errors
/// As [`apply_as`].
pub fn apply_at(
    doc: &mut AutoCommit,
    author: &str,
    now_ms: i64,
    invoke: Invoke<Chat>,
) -> Result<(), DoorError> {
    initialized(doc).map_err(|error| DoorError::Failed(error.to_string()))?;
    let mut replica = Replica::<Chat>::from_doc(std::mem::take(doc))
        .as_invoker(author)
        .in_app(<crate::ChatShell as ui::framework::ResourceApp>::ID, "Chat");
    replica.set_now_ms(now_ms);
    // Asked for by name, as a person confirming it would.
    let applied = replica.apply_confirmed(invoke).map(drop);
    std::mem::swap(doc, replica.doc_mut());
    applied
}

/// Start the chat in `doc` as `me` would: the room's `#chat` and `#voice`
/// when it has no channels yet, and `me` in it.
///
/// # Errors
/// As [`apply_as`].
pub fn start(doc: &mut AutoCommit, me: &str, name: &str, initials: &str) -> Result<(), DoorError> {
    apply_as(
        doc,
        me,
        Chat::BEGIN_CHAT.with(chat::BeginChat {
            name: name.into(),
            initials: initials.into(),
            color: 0,
        }),
    )
}

/// Introduce `me` into the chat in `doc`.
///
/// # Errors
/// As [`apply_as`].
pub fn introduce(
    doc: &mut AutoCommit,
    me: &str,
    name: &str,
    initials: &str,
) -> Result<(), DoorError> {
    apply_as(
        doc,
        me,
        Chat::INTRODUCE.with(chat::Introduce {
            name: name.into(),
            initials: initials.into(),
            color: 1,
        }),
    )
}

/// `post` of `text` in `channel`, optionally in the thread of `parent`.
#[must_use]
pub fn post(channel: &str, text: &str, parent: Option<&str>) -> Invoke<ChatPart> {
    ChatPart::POST.with(chat_part::Post {
        message: crate::messages::NewMessage {
            channel: Id::from_raw(channel),
            text: text.into(),
            parent: parent.map(Id::from_raw),
            body: None,
            attachment: None,
            mentions: None,
            written: None,
        },
    })
}

/// Apply `invoke`, a verb on the messages, as `author` invokes it to
/// `part`, a part of the chat's messages held as a document (started with
/// its genesis when empty), read against the chat record `chat` as the
/// window's chain reads it: for a tool or a host that carries the parts as
/// documents itself.
///
/// # Errors
/// The chat record's refusal, or the door's or the verb's own.
pub fn apply_part_doc(
    part: &mut AutoCommit,
    chat: &Chat,
    author: &str,
    invoke: impl Into<Invoke<ChatPart>>,
) -> Result<(), DoorError> {
    use automerge::ReadDoc as _;
    use ui::framework::Chained as _;
    let invoke = invoke.into();
    if let Some(refusal) = invoke.refusal() {
        return Err(DoorError::InvalidArguments(refusal.to_owned()));
    }
    if part
        .get(automerge::ROOT, "messages")
        .map_err(|error| DoorError::Failed(error.to_string()))?
        .is_none()
    {
        <ChatPart as ui::tilo_resource::Resource>::initialize(part)
            .map_err(|error| DoorError::Failed(error.to_string()))?;
        commit_genesis(part);
    }
    let mut action = invoke
        .build(ui::tilo_resource::origin_now_ms())
        .map_err(DoorError::InvalidArguments)?;
    ChatPart::prepare(&mut action, chat).map_err(DoorError::Failed)?;
    let mut replica = Replica::<ChatPart>::from_doc(std::mem::take(part))
        .as_invoker(author)
        .in_app(<crate::ChatShell as ui::framework::ResourceApp>::ID, "Chat");
    let applied = replica.apply_action(&action).map(drop);
    std::mem::swap(part, replica.doc_mut());
    applied
}

/// Post `text` in `channel` as `author` into `part`, as
/// [`apply_part_doc`] applies it; the new message's id.
///
/// # Errors
/// As [`apply_part_doc`].
pub fn say_in_part(
    part: &mut AutoCommit,
    chat: &Chat,
    author: &str,
    channel: &str,
    text: &str,
) -> Result<String, DoorError> {
    let ids = |part: &AutoCommit| -> Vec<Id> {
        crate::messages::read_part(part)
            .messages
            .ids()
            .cloned()
            .collect()
    };
    let before = ids(part);
    apply_part_doc(part, chat, author, post(channel, text, None))?;
    Ok(ids(part)
        .into_iter()
        .find(|id| !before.contains(id))
        .map(|id| id.to_string())
        .unwrap_or_default())
}

/// Post `text` in `channel` of `room` as `author`; the new message's id.
///
/// # Errors
/// As [`Room::apply_part_as`].
pub fn say(room: &mut Room, author: &str, channel: &str, text: &str) -> Result<String, DoorError> {
    room.say(author, channel, text)
}

/// Add the category `id` named `name` to `doc`, as `author`.
///
/// # Errors
/// As [`apply_as`].
pub fn insert_category(
    doc: &mut AutoCommit,
    author: &str,
    id: &str,
    name: &str,
) -> Result<(), DoorError> {
    let at = crate::projection::read(doc).categories.len();
    let category = Category { name: name.into() };
    let insert = Chat::paths()
        .categories
        .insert_as(&Id::from_raw(id), at, &category);
    let insert =
        Invoke::edit(&insert).map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    apply_as(doc, author, insert)
}

/// Add the channel `id` named `name` under `category` (none when empty) of
/// `doc`, as `author`.
///
/// # Errors
/// As [`apply_as`].
pub fn insert_channel(
    doc: &mut AutoCommit,
    author: &str,
    id: &str,
    category: &str,
    name: &str,
    kind: ChannelKind,
) -> Result<(), DoorError> {
    let at = crate::projection::read(doc).channels.len();
    let channel = Channel {
        category: (!category.is_empty()).then(|| Id::from_raw(category)),
        name: name.into(),
        kind,
        ..Channel::default()
    };
    let insert = Chat::paths()
        .channels
        .insert_as(&Id::from_raw(id), at, &channel);
    let insert =
        Invoke::edit(&insert).map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    apply_as(doc, author, insert)
}
