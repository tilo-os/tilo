//! Test fixtures over a chat held without a window ([`Room`]): the demo and
//! starter records, verbs applied as a named person, and the record and its
//! messages read back as the window reads them.

use automerge::AutoCommit;
use ui::tilo_resource::{Id, Invoke};

pub(crate) use crate::demo::{Room, apply_as, apply_at, post};
use crate::model::{Channel, Message, User};
use crate::record::chat;
use crate::resource::Chat;

/// A room holding the demo room, read by [`crate::model::ME`].
pub(crate) fn demo_doc() -> Room {
    Room::demo().unwrap()
}

/// The demo room's chat record alone, read by [`crate::model::ME`]:
/// for what reads no messages.
pub(crate) fn demo_chat() -> AutoCommit {
    let mut doc = AutoCommit::new();
    crate::demo::seed(&mut doc).unwrap();
    doc
}

/// A verb on the messages of `room`, applied as `author` invokes it at
/// `now_ms`; the part it changed.
pub(crate) fn apply_part_at(
    room: &mut Room,
    author: &str,
    now_ms: i64,
    invoke: Invoke<crate::parts::ChatPart>,
) -> Result<u64, ui::tilo_actions::DoorError> {
    room.apply_part_at(author, now_ms, invoke)
}

/// A verb on the messages of `room`, applied as `author` invokes it; the
/// part it changed.
pub(crate) fn apply_part_as(
    room: &mut Room,
    author: &str,
    invoke: Invoke<crate::parts::ChatPart>,
) -> Result<u64, ui::tilo_actions::DoorError> {
    room.apply_part_as(author, invoke)
}

/// Start the chat in `doc` as `me` would: the room's `#chat` and `#voice`.
pub(crate) fn starter(doc: &mut AutoCommit, me: &str, name: &str, initials: &str) {
    crate::demo::start(doc, me, name, initials).unwrap();
}

pub(crate) fn introduce(doc: &mut AutoCommit, me: &str, name: &str, initials: &str) {
    crate::demo::introduce(doc, me, name, initials).unwrap();
}

/// Post `text` in `channel` as `author`; the new message's id.
pub(crate) fn say(doc: &mut Room, author: &str, channel: &str, text: &str) -> String {
    doc.say(author, channel, text).unwrap()
}

/// Write `text` in `app`'s main composer, as its person typing it: the
/// draft is theirs, never in `doc`.
pub(crate) fn write_draft(app: &mut crate::app::ChatApp, doc: &mut AutoCommit, text: &str) {
    app.reload(doc);
    app.composer_main.ensure_draft(doc);
    app.composer_main
        .insert_text(&mut ui::widget::Edits::lend(doc), text);
}

/// The messages of `channel`, oldest first.
pub(crate) fn messages(doc: &Room, channel: &str) -> Vec<Message> {
    doc.message_rows(channel)
}

/// The parts of the chat's messages a mount's store holds.
pub(crate) fn stored_messages(records: &ui::record::RecordStore) -> crate::messages::Messages {
    use ui::tilo_resource::Resource as _;
    crate::messages::Messages::new(
        records
            .held_parts(&crate::parts::chain())
            .into_iter()
            .filter_map(|index| {
                let part = records
                    .read(&crate::parts::part(index), |doc| {
                        crate::parts::ChatPart::read(doc).ok()
                    })
                    .flatten()?;
                Some((index, std::rc::Rc::new(part)))
            })
            .collect(),
    )
}

/// The messages of `channel` a mount's store holds, oldest first.
pub(crate) fn messages_in(records: &ui::record::RecordStore, channel: &str) -> Vec<Message> {
    let chat = records
        .read(
            &ui::record::RecordId::package_primary(),
            crate::projection::read,
        )
        .flatten()
        .unwrap_or_default();
    chat.message_rows(&stored_messages(records), channel)
}

/// How many top-level messages of `channel` a mount's store holds.
pub(crate) fn top_level_in(records: &ui::record::RecordStore, channel: &str) -> usize {
    stored_messages(records).top_level(&Id::from_raw(channel))
}

/// The parts of the chat's messages a mounted app's window holds.
pub(crate) fn mounted_messages(
    app: &mut ui::framework::testing::TestApp<crate::ChatShell>,
) -> crate::messages::Messages {
    app.with_surface::<crate::ChatApp, _>(|surface| surface.held_messages())
        .unwrap_or_default()
}

/// The messages of `channel` a mounted app's window shows, oldest first.
pub(crate) fn mounted_rows(
    app: &mut ui::framework::testing::TestApp<crate::ChatShell>,
    channel: &str,
) -> Vec<Message> {
    let held = mounted_messages(app);
    app.state().message_rows(&held, channel)
}

/// Post `text` in `channel` through a mounted app's door, as its person.
pub(crate) fn mounted_post(
    app: &mut ui::framework::testing::TestApp<crate::ChatShell>,
    channel: &str,
    text: &str,
) {
    app.call_on(
        &chain(),
        "post",
        serde_json::json!({ "channel": channel, "text": text }),
    )
    .expect("the post is applied");
}

/// The address of the chat's messages, as a window publishes them.
pub(crate) fn chain() -> String {
    ui::framework::chain_resource_id(&crate::parts::chain())
}

/// The id of the item the chain publishes for the message `id`.
pub(crate) fn message_item(id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<crate::parts::ChatPart>(
        "messages",
        &Id::from_raw(id),
    )
}

/// The channels, in sidebar order.
pub(crate) fn channels(doc: &AutoCommit) -> Vec<Channel> {
    crate::projection::read(doc)
        .expect("a chat record")
        .channel_rows()
}

/// The people in the room.
pub(crate) fn users(doc: &AutoCommit) -> Vec<User> {
    crate::projection::read(doc)
        .expect("a chat record")
        .user_rows()
}

/// How many top-level messages `channel` has.
pub(crate) fn top_level(doc: &Room, channel: &str) -> usize {
    doc.messages().top_level(&Id::from_raw(channel))
}

pub(crate) fn read_mark(doc: &AutoCommit, channel: &str, user: &str) -> crate::parts::ReadMark {
    crate::projection::read(doc)
        .expect("a chat record")
        .read_mark(channel, user)
}

pub(crate) fn draft_text(doc: &AutoCommit, key: &str) -> String {
    crate::projection::draft_text(doc, key)
}

/// The id of the item resource the record publishes for `id` of `field`.
pub(crate) fn item(field: &str, id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<Chat>(field, &Id::from_raw(id))
}

/// The operation `invoke` builds, as JSON to compare with what a window
/// dispatched.
pub(crate) fn operation(invoke: &Invoke<Chat>) -> serde_json::Value {
    serde_json::to_value(invoke.build(0).expect("the invocation builds").operation)
        .expect("an operation encodes")
}

/// Add the category `id` named `name` to `doc`.
pub(crate) fn insert_category(doc: &mut AutoCommit, id: &str, name: &str) {
    crate::demo::insert_category(doc, id, name).unwrap();
}

/// Add the channel `id` named `name` under `category` of `doc`.
pub(crate) fn insert_channel(
    doc: &mut AutoCommit,
    id: &str,
    category: &str,
    name: &str,
    kind: crate::model::ChannelKind,
) {
    crate::demo::insert_channel(doc, id, category, name, kind).unwrap();
}

/// The published report `id`, carrying the reads a window offers on it.
pub(crate) fn report_resource(id: &str, text: &str) -> ui::tilo_actions::Resource {
    ui::tilo_actions::Resource::new(item("feedback", id), "chat.workspace.feedback", text)
        .with_action(crate::feedback::receipt_read::spec(id))
        .with_action(crate::feedback::status_read::spec(id))
}

/// `submit-feedback` of `submission` in the channel it names.
pub(crate) fn submit_feedback(submission: &ui::feedback::Submission) -> Invoke<Chat> {
    Chat::SUBMIT_FEEDBACK.with(chat::SubmitFeedback {
        channel: Id::from_raw(submission.destination.channel.clone()),
        submission: serde_json::to_value(submission).expect("a submission encodes"),
    })
}

/// The demo agent's reply to the `agent-work` summons.
pub(crate) fn agent_reply() -> String {
    crate::record::reply_id(
        &Id::from_raw(crate::resource::DEMO_SUMMONS),
        crate::resource::DEMO_AGENT,
    )
    .to_string()
}

pub(crate) fn record_work(
    summons: &str,
    work: &crate::model::AgentWork,
) -> Invoke<crate::parts::ChatPart> {
    crate::parts::ChatPart::RECORD_WORK.with(crate::messages::chat_part::RecordWork {
        message: Id::from_raw(summons),
        work: work_update(work),
        say: None,
    })
}

/// The id of the call row of the voice channel `channel`.
pub(crate) fn call_row(channel: &str) -> String {
    format!("{}/call", item("channels", channel))
}

pub(crate) fn work_update(work: &crate::model::AgentWork) -> crate::record::WorkUpdate {
    use crate::model::{AgentRoute, AgentStepOutcome, AgentWorkState};

    let (state, app, did, about) = match &work.state {
        AgentWorkState::Taken => ("taken", None, None, None),
        AgentWorkState::Waiting { app } => ("waiting", Some(app.clone()), None, None),
        AgentWorkState::Doing { did, about } => {
            ("doing", None, Some(did.clone()), Some(about.clone()))
        }
        AgentWorkState::Done { did } => ("done", None, Some(did.clone()), None),
        AgentWorkState::Refused => ("refused", None, None, None),
        AgentWorkState::ProviderFinished { outcome } => {
            use crate::agent_stream::AgentFinish;
            let state = match outcome {
                AgentFinish::Complete => "provider_complete",
                AgentFinish::Truncated => "provider_truncated",
                AgentFinish::Refused => "provider_refused",
                AgentFinish::Cancelled => "provider_cancelled",
                AgentFinish::Failed => "provider_failed",
            };
            (state, None, None, None)
        }
    };
    crate::record::WorkUpdate {
        state: state.into(),
        app,
        did,
        about,
        route: work.reasoning.as_ref().map(|reasoning| {
            match reasoning.route {
                AgentRoute::Model => "model",
                AgentRoute::Floor => "floor",
            }
            .to_owned()
        }),
        why: work
            .reasoning
            .as_ref()
            .map(|reasoning| reasoning.why.clone()),
        trace: work
            .trace
            .iter()
            .map(|step| {
                let (outcome, why) = match &step.outcome {
                    AgentStepOutcome::Asked => ("asked", None),
                    AgentStepOutcome::Applied => ("applied", None),
                    AgentStepOutcome::NoChange => ("no_change", None),
                    AgentStepOutcome::Refused { why } => ("refused", Some(why.clone())),
                };
                crate::record::StepUpdate {
                    id: step.id.clone(),
                    sequence: step.sequence,
                    resource: step.resource.clone(),
                    label: step.label.clone(),
                    outcome: outcome.into(),
                    why,
                }
            })
            .collect(),
    }
}

/// A chat from before Chat's record was a model, as it was stored at
/// `version`: each category's and channel's fields flat in its entry. At
/// version 5 an order key sits beside them and the item holds an empty name
/// and topic, written over the fields it could not read; at version 1 it
/// holds neither.
pub(crate) fn chat_kept_flat(version: u64) -> AutoCommit {
    use serde_json::json;
    use ui::tilo_resource::{Slot, evolution};
    let opened = version >= 5;
    let item = |key: &str, texts: &[&str]| {
        let mut entry = serde_json::Map::new();
        if opened {
            entry.insert("k".into(), json!(key));
            let held: serde_json::Map<_, _> = texts
                .iter()
                .map(|text| ((*text).to_owned(), json!({ "$text": "" })))
                .collect();
            entry.insert("v".into(), held.into());
        }
        entry
    };
    let with = |mut entry: serde_json::Map<String, serde_json::Value>,
                fields: serde_json::Value| {
        if let serde_json::Value::Object(fields) = fields {
            entry.extend(fields);
        }
        serde_json::Value::Object(entry)
    };
    let mut doc = AutoCommit::new();
    evolution::fixture(
        &mut doc,
        &automerge::ROOT,
        &json!({
            "categories": {
                "cat_home_text": with(item("i", &["name"]), json!({
                    "workspace": "ws_home", "name": "Channels", "order": 0,
                })),
                "cat_home_voice": with(item("", &["name"]), json!({
                    "workspace": "ws_home", "name": "Voice", "order": 1,
                })),
            },
            "channels": {
                "c_general": with(item("i", &["name", "topic"]), json!({
                    "workspace": "ws_home", "category": "cat_home_text",
                    "name": "general", "topic": "", "kind": "text", "order": 0,
                })),
                "c_lounge": with(item("", &["name", "topic"]), json!({
                    "workspace": "ws_home", "category": "cat_home_voice",
                    "name": "Lounge", "topic": "", "kind": "voice", "order": 0,
                })),
            },
        }),
    )
    .unwrap();
    if version > 1 {
        Slot::Key(&automerge::ROOT, evolution::VERSION_KEY)
            .put_scalar(&mut doc, automerge::ScalarValue::Uint(version))
            .unwrap();
    }
    doc.commit();
    doc
}

/// The participant id of bo, whom no record names.
pub(crate) const BO: &str = "5f0c2a9e7d41b36c8e9fa1d2047b5c3e6a8d9f10b2c4e6a7d8f9012b3c4d5e6f";

/// A chat record of `version` that kept its messages in itself: `#general`
/// with `first` by ada and `second` by [`BO`], `#random` with `elsewhere`
/// by ada between them. Versions 1 and 2 kept them as a collection; one
/// from before the record was a model (`flat`) as a list per channel, and
/// ada's name, Ada Lovelace, under `users`.
pub(crate) fn chat_with_kept_messages(version: u64, flat: bool) -> AutoCommit {
    use serde_json::json;
    use ui::tilo_resource::{Slot, evolution};
    let channel = |key: &str, name: &str| json!({ "k": key, "v": { "name": { "$text": name } } });
    let messages = if flat {
        let row = |id: &str, author: &str, ts: i64, text: &str| json!({ "id": id, "author": author, "ts": ts, "text": { "$text": text } });
        json!({
            "c_general": [row("m1", "ada", 10, "first"), row("m2", BO, 20, "second")],
            "c_random": [row("m3", "ada", 15, "elsewhere")],
        })
    } else {
        let item = |key: &str, channel: &str, author: &str, ts: i64, text: &str| json!({ "k": key, "v": { "channel": channel, "author": author, "ts": ts, "text": text } });
        json!({
            "m1": item("a", "c_general", "ada", 10, "first"),
            "m2": item("b", "c_general", BO, 20, "second"),
            "m3": item("c", "c_random", "ada", 15, "elsewhere"),
        })
    };
    let mut doc = AutoCommit::new();
    evolution::fixture(
        &mut doc,
        &automerge::ROOT,
        &json!({
            "channels": { "c_general": channel("a", "general"), "c_random": channel("b", "random") },
            "messages": messages,
            "users": if flat { json!({ "ada": { "name": "Ada Lovelace" } }) } else { json!({}) },
        }),
    )
    .unwrap();
    Slot::Key(&automerge::ROOT, evolution::VERSION_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Uint(version))
        .unwrap();
    doc.commit();
    doc
}
