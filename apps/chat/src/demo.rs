//! The demo workspace a screenshot or a test shows: its people, rooms and
//! conversation, read by `me`. Never a real record's content.

use automerge::AutoCommit;
use ui::tilo_actions::DoorError;
use ui::tilo_resource::{Atom, Edit, Id, Invoke, Json, ModelError, Replica, Set};

use crate::model::{Attachment, ChannelKind, EditRecord, Presence};
use crate::record::{Category, Channel, Chat, Message, Person, Revision, Workspace, chat};

/// Deterministic "now" for the demo so a screenshot is stable: 2026-06-30
/// 10:40 UTC, just before the capture clock's 10:42.
pub const NOW: i64 = 1_782_470_400;

/// One row of the demo channel roster: (id, workspace, category, name,
/// topic, kind, sort order within its category).
type DemoChannelSeed = (
    &'static str,
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
                "ws_acme",
                "cat_acme_main",
                "general",
                "Company-wide announcements",
                ChannelKind::Text,
                0,
            ),
            (
                "c_crit",
                "ws_acme",
                "cat_acme_main",
                "design-crit",
                "Share work, get sharp feedback",
                ChannelKind::Text,
                1,
            ),
            (
                "c_frontend",
                "ws_acme",
                "cat_acme_main",
                "frontend",
                "ui toolkit + app shells",
                ChannelKind::Text,
                2,
            ),
            (
                "c_random",
                "ws_acme",
                "cat_acme_main",
                "random",
                "Off-topic & watercooler",
                ChannelKind::Text,
                3,
            ),
            (
                "c_standup",
                "ws_acme",
                "cat_acme_voice",
                "Daily Standup",
                "",
                ChannelKind::Voice,
                0,
            ),
            (
                "c_huddle",
                "ws_acme",
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
                "ws_tilo",
                "cat_tilo_eng",
                "kernel",
                "bootc + immutable base",
                ChannelKind::Text,
                0,
            ),
            (
                "c_compositor",
                "ws_tilo",
                "cat_tilo_eng",
                "compositor",
                "2D clusters & sandboxing",
                ChannelKind::Text,
                1,
            ),
            (
                "c_fssync",
                "ws_tilo",
                "cat_tilo_eng",
                "fs-sync",
                "iroh projection & hydration",
                ChannelKind::Text,
                2,
            ),
            (
                "c_releases",
                "ws_tilo",
                "cat_tilo_eng",
                "releases",
                "ship logs",
                ChannelKind::Text,
                3,
            ),
            (
                "c_warroom",
                "ws_tilo",
                "cat_tilo_voice",
                "War Room",
                "",
                ChannelKind::Voice,
                0,
            ),
            (
                "c_ideas",
                "ws_jam",
                "cat_jam_main",
                "ideas",
                "weekend hacks",
                ChannelKind::Text,
                0,
            ),
            (
                "c_music",
                "ws_jam",
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

/// The demo workspace, with `me` as the person reading it.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the demo's people, rooms and conversation, written out as data"
)]
pub fn demo(me: &str) -> Chat {
    let mut chat = Chat::default();
    for (id, name, initials, color, status) in [
        (me, "You", "QD", 0u32, Presence::Online),
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
    for (id, name, glyph, color) in [
        ("ws_acme", "Acme Design", "AD", 0u32),
        ("ws_tilo", "Tilo Core", "QC", 1),
        ("ws_jam", "Weekend Jam", "WJ", 3),
    ] {
        chat.workspaces.push(
            Id::from_raw(id),
            Workspace {
                name: name.to_owned(),
                glyph: glyph.to_owned(),
                color,
            },
        );
    }
    for (id, workspace, name) in [
        ("cat_acme_main", "ws_acme", "Channels"),
        ("cat_acme_voice", "ws_acme", "Voice"),
        ("cat_tilo_eng", "ws_tilo", "Engineering"),
        ("cat_tilo_voice", "ws_tilo", "Voice"),
        ("cat_jam_main", "ws_jam", "Hangout"),
    ] {
        chat.categories.push(
            Id::from_raw(id),
            Category {
                workspace: Id::from_raw(workspace),
                name: name.to_owned(),
            },
        );
    }
    for (id, workspace, category, name, topic, kind, _) in Demo::channels_acme()
        .iter()
        .chain(Demo::channels_tilo_and_jam())
    {
        chat.open_channel(
            Id::from_raw(*id),
            Channel {
                workspace: Id::from_raw(*workspace),
                category: Some(Id::from_raw(*category)),
                name: (*name).to_owned(),
                topic: (*topic).to_owned(),
                kind: *kind,
                archived: false,
            },
        );
    }
    let mine = |id: &str| {
        if id == crate::model::ME {
            me.to_owned()
        } else {
            id.to_owned()
        }
    };
    let post = |chat: &mut Chat, id: String, channel: &str, message: NewMessage| {
        let mut reactions = Set::new();
        for (emoji, users) in message.reactions {
            for user in users {
                reactions.insert(crate::record::Reacted {
                    emoji: emoji.clone(),
                    user: mine(&user),
                });
            }
        }
        chat.messages.push(
            Id::from_raw(id),
            Message {
                channel: Id::from_raw(channel),
                author: Atom(mine(&message.author)),
                ts: message.ts,
                mentions: Json(crate::mention::resolve(&message.text, &chat.participants())),
                text: message.text,
                edited: message.edited,
                history: Json(
                    message
                        .history
                        .into_iter()
                        .map(|record| Revision {
                            ts: record.ts,
                            text: record.text,
                        })
                        .collect(),
                ),
                parent: message.parent.map(Id::from_raw),
                reactions,
                attachment: Json(message.attachment),
                ..Message::default()
            },
        );
    };
    let mut crit = Demo::design_crit_early();
    crit.extend(Demo::design_crit_late());
    for (id, message) in crit {
        post(&mut chat, id.to_owned(), "c_crit", message);
    }
    for (index, message) in Demo::design_crit_thread("m_concept")
        .into_iter()
        .enumerate()
    {
        post(&mut chat, format!("m_thread_{index}"), "c_crit", message);
    }
    post(
        &mut chat,
        "m_welcome".to_owned(),
        "c_general",
        Demo::general_welcome(),
    );
    chat
}

/// `chat` written into an empty record, item by item: what a capture's
/// scenario and a test start from.
#[must_use]
pub fn edits(chat: &Chat) -> Vec<Edit<Chat>> {
    let paths = Chat::paths();
    let mut edits = Vec::new();
    for (at, (id, item)) in chat.workspaces.iter().enumerate() {
        edits.push(paths.workspaces.insert_as(id, at, item));
    }
    for (at, (id, item)) in chat.categories.iter().enumerate() {
        edits.push(paths.categories.insert_as(id, at, item));
    }
    for (at, (id, item)) in chat.channels.iter().enumerate() {
        edits.push(paths.channels.insert_as(id, at, item));
    }
    for (at, (id, item)) in chat.messages.iter().enumerate() {
        edits.push(paths.messages.insert_as(id, at, item));
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
    if !chat.drafts.is_empty() {
        edits.push(paths.drafts.set(chat.drafts.clone()));
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
    if doc.get(automerge::ROOT, "workspaces")?.is_none() {
        <Chat as ui::tilo_resource::Resource>::initialize(doc)?;
        commit_genesis(doc);
    }
    Ok(())
}

/// The demo workspace in `doc`, read by [`crate::model::ME`].
///
/// # Errors
/// When `doc` already holds a chat the demo does not fit into.
pub fn seed(doc: &mut AutoCommit) -> Result<(), ModelError> {
    write(doc, &demo(crate::model::ME))
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
    let mut replica = Replica::<Chat>::from_doc(std::mem::take(doc)).as_invoker(author);
    replica.set_now_ms(now_ms);
    // Asked for by name, as a person confirming it would.
    let applied = replica.apply_confirmed(invoke).map(drop);
    std::mem::swap(doc, replica.doc_mut());
    applied
}

/// Start the chat in `doc` as `me` would: their starter workspace.
///
/// # Errors
/// As [`apply_as`].
pub fn start(doc: &mut AutoCommit, me: &str, name: &str, initials: &str) -> Result<(), DoorError> {
    apply_as(
        doc,
        me,
        Chat::BEGIN_CHAT.with(chat::BeginChat {
            workspace: None,
            workspace_name: None,
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
pub fn post(channel: &str, text: &str, parent: Option<&str>) -> Invoke<Chat> {
    Chat::POST.with(chat::Post {
        channel: Id::from_raw(channel),
        text: text.into(),
        parent: parent.map(Id::from_raw),
        body: None,
        attachment: None,
    })
}

/// Post `text` in `channel` as `author`; the new message's id.
///
/// # Errors
/// As [`apply_as`].
pub fn say(
    doc: &mut AutoCommit,
    author: &str,
    channel: &str,
    text: &str,
) -> Result<String, DoorError> {
    let messages = |doc: &AutoCommit| crate::projection::read(doc).message_rows(channel);
    let before: Vec<String> = messages(doc).into_iter().map(|m| m.id).collect();
    apply_as(doc, author, post(channel, text, None))?;
    Ok(messages(doc)
        .into_iter()
        .map(|message| message.id)
        .find(|id| !before.contains(id))
        .unwrap_or_default())
}

/// Replace the draft `key` with `text`, starting it first, as `author`.
///
/// # Errors
/// As [`apply_as`].
pub fn set_draft(
    doc: &mut AutoCommit,
    author: &str,
    key: &str,
    text: &str,
) -> Result<(), DoorError> {
    apply_as(
        doc,
        author,
        Chat::START_DRAFT.with(chat::StartDraft { key: key.into() }),
    )?;
    let replace = Chat::paths()
        .drafts
        .key(key)
        .perform(&tilo_document::RichTextAction::ReplaceBody {
            markdown: text.into(),
        })
        .and_then(|edit| Invoke::edit(&edit))
        .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    apply_as(doc, author, replace)
}

/// Start the chat in `doc` in the workspace `workspace` named `label` a
/// host admitted it for, as `me` named `name`.
///
/// # Errors
/// As [`apply_as`].
pub fn start_in(
    doc: &mut AutoCommit,
    me: &str,
    (workspace, label): (&str, &str),
    name: &str,
    initials: &str,
) -> Result<(), DoorError> {
    apply_as(
        doc,
        me,
        Chat::BEGIN_CHAT.with(chat::BeginChat {
            workspace: Some(workspace.into()),
            workspace_name: Some(label.into()),
            name: name.into(),
            initials: initials.into(),
            color: 0,
        }),
    )
}

/// Add the category `id` named `name` to `workspace` of `doc`, as `author`.
///
/// # Errors
/// As [`apply_as`].
pub fn insert_category(
    doc: &mut AutoCommit,
    author: &str,
    id: &str,
    workspace: &str,
    name: &str,
) -> Result<(), DoorError> {
    let at = crate::projection::read(doc).categories.len();
    let category = Category {
        workspace: Id::from_raw(workspace),
        name: name.into(),
    };
    let insert = Chat::paths()
        .categories
        .insert_as(&Id::from_raw(id), at, &category);
    let insert =
        Invoke::edit(&insert).map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    apply_as(doc, author, insert)
}

/// Add the channel `id` named `name` to `(workspace, category)` of `doc`,
/// as `author`; a text channel with its draft.
///
/// # Errors
/// As [`apply_as`].
pub fn insert_channel(
    doc: &mut AutoCommit,
    author: &str,
    id: &str,
    (workspace, category): (&str, &str),
    name: &str,
    kind: ChannelKind,
) -> Result<(), DoorError> {
    let at = crate::projection::read(doc).channels.len();
    let channel = Channel {
        workspace: Id::from_raw(workspace),
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
    apply_as(doc, author, insert)?;
    if kind == ChannelKind::Text {
        apply_as(
            doc,
            author,
            Chat::START_DRAFT.with(chat::StartDraft { key: id.into() }),
        )?;
    }
    Ok(())
}

/// Add `message` to `doc` under `id`, as its author wrote it at its time:
/// a conversation a test or a tool starts from.
///
/// # Errors
/// As [`apply_as`].
pub fn insert_message(doc: &mut AutoCommit, id: &str, message: &Message) -> Result<(), DoorError> {
    let at = crate::projection::read(doc).messages.len();
    let insert = Chat::paths()
        .messages
        .insert_as(&Id::from_raw(id), at, message);
    let insert =
        Invoke::edit(&insert).map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    apply_as(doc, &message.author.0, insert)
}
