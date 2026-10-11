//! Test fixtures over a bare chat document: the demo and starter records,
//! verbs applied as a named person, and the record read back as the window
//! reads it.

use automerge::AutoCommit;
use ui::tilo_resource::{Id, Invoke};

pub(crate) use crate::demo::{apply_as, apply_at, post};
use crate::model::{Channel, Message, User};
use crate::record::chat;
use crate::resource::Chat;

/// A document holding the demo workspace, read by [`crate::model::ME`].
pub(crate) fn demo_doc() -> AutoCommit {
    let mut doc = AutoCommit::new();
    crate::demo::seed(&mut doc).unwrap();
    doc
}

/// Start the chat in `doc` as `me` would: their starter workspace.
pub(crate) fn starter(doc: &mut AutoCommit, me: &str, name: &str, initials: &str) {
    crate::demo::start(doc, me, name, initials).unwrap();
}

/// Introduce `me` into the chat in `doc`.
pub(crate) fn introduce(doc: &mut AutoCommit, me: &str, name: &str, initials: &str) {
    crate::demo::introduce(doc, me, name, initials).unwrap();
}

/// Post `text` in `channel` as `author`; the new message's id.
pub(crate) fn say(doc: &mut AutoCommit, author: &str, channel: &str, text: &str) -> String {
    crate::demo::say(doc, author, channel, text).unwrap()
}

/// Replace the draft `key` with `text`, starting it first.
pub(crate) fn set_draft(doc: &mut AutoCommit, key: &str, text: &str) {
    crate::demo::set_draft(doc, crate::model::ME, key, text).unwrap();
}

/// The messages of `channel`, oldest first.
pub(crate) fn messages(doc: &AutoCommit, channel: &str) -> Vec<Message> {
    crate::projection::read(doc).message_rows(channel)
}

/// The channels, in sidebar order.
pub(crate) fn channels(doc: &AutoCommit) -> Vec<Channel> {
    crate::projection::read(doc).channel_rows()
}

/// The people in the room.
pub(crate) fn users(doc: &AutoCommit) -> Vec<User> {
    crate::projection::read(doc).user_rows()
}

/// How many top-level messages `channel` has.
pub(crate) fn top_level(doc: &AutoCommit, channel: &str) -> usize {
    crate::projection::read(doc).top_level(&Id::from_raw(channel))
}

/// `user`'s read mark in `channel`.
pub(crate) fn read_mark(doc: &AutoCommit, channel: &str, user: &str) -> i64 {
    crate::projection::read(doc).read_mark(channel, user)
}

/// The draft `key`'s text.
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

/// Add the category `id` named `name` to `workspace` of `doc`.
pub(crate) fn insert_category(doc: &mut AutoCommit, id: &str, workspace: &str, name: &str) {
    crate::demo::insert_category(doc, crate::model::ME, id, workspace, name).unwrap();
}

/// Add the channel `id` named `name` to `category` of `workspace` of `doc`.
pub(crate) fn insert_channel(
    doc: &mut AutoCommit,
    id: &str,
    workspace: &str,
    category: &str,
    name: &str,
    kind: crate::model::ChannelKind,
) {
    crate::demo::insert_channel(doc, crate::model::ME, id, (workspace, category), name, kind)
        .unwrap();
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

/// `record-work` of `work` on the message `summons`.
pub(crate) fn record_work(summons: &str, work: &crate::model::AgentWork) -> Invoke<Chat> {
    Chat::RECORD_WORK.with(chat::RecordWork {
        message: Id::from_raw(summons),
        work: work.to_update(),
        say: None,
    })
}

/// The id of the call row of the voice channel `channel`.
pub(crate) fn call_row(channel: &str) -> String {
    format!("{}/call", item("channels", channel))
}
