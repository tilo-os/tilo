//! Test fixtures over a chat held without a window ([`Workspace`]): the demo and
//! starter resources, verbs applied as a named person, and the resource and its
//! messages read back as the window reads them.

use crate::projection::ChatProjection as _;
use automerge::AutoCommit;
use ui::tilo_resource::{Id, Invoke};

use crate::chat::chat;
pub(crate) use crate::demo::{Workspace, apply_as, apply_at, post};
use crate::model::{Channel, Message, User};
use crate::resource::Chat;

/// A workspace holding the demo workspace, read by [`crate::model::ME`].
pub(crate) fn demo_doc() -> Workspace {
    Workspace::demo().unwrap()
}

/// The demo workspace's chat resource alone, read by [`crate::model::ME`]:
/// for what reads no messages.
pub(crate) fn demo_chat() -> AutoCommit {
    let mut doc = AutoCommit::new();
    crate::demo::seed(&mut doc).unwrap();
    doc
}

/// A verb on the messages of `workspace`, applied as `author` invokes it at
/// `now_ms`; the part it changed.
pub(crate) fn apply_part_at(
    workspace: &mut Workspace,
    author: &str,
    now_ms: i64,
    invoke: Invoke<crate::parts::ChatMessages>,
) -> Result<u64, ui::tilo_actions::ApiError> {
    workspace.apply_part_at(&ui::tilo_resource::MemberId::person(author), now_ms, invoke)
}

/// A verb on the messages of `workspace`, applied as `author` invokes it; the
/// part it changed.
pub(crate) fn apply_part_as(
    workspace: &mut Workspace,
    author: &str,
    invoke: Invoke<crate::parts::ChatMessages>,
) -> Result<u64, ui::tilo_actions::ApiError> {
    workspace.apply_part_as(&ui::tilo_resource::MemberId::person(author), invoke)
}

/// Start the chat in `doc` as `me`, called `name`, would: the workspace's `#chat` and `#voice`.
pub(crate) fn starter(doc: &mut AutoCommit, me: &str, name: &str) {
    crate::demo::start(doc, me).unwrap();
    member(me, name);
}

/// The person `me` is a member of the workspace, called `name`, as its member list says.
pub(crate) fn member(me: &str, name: &str) {
    let me = ui::tilo_resource::MemberId::person(me);
    let mut members = ui::tilo_resource::members::named();
    members.retain(|(id, _)| *id != me);
    members.push((me, name.to_owned()));
    ui::tilo_resource::members::know(members);
}

/// The agent `agent` is a member of the workspace, called `name`, as the member list `app` and
/// the record's code receive says.
pub(crate) fn agent_member(app: &mut crate::app::ChatApp, agent: &str, name: &str) {
    let agent = ui::tilo_resource::MemberId::agent(agent);
    let mut members = ui::tilo_resource::members::named();
    members.retain(|(id, _)| *id != agent);
    members.push((agent.clone(), name.to_owned()));
    ui::tilo_resource::members::know(members);
    app.workspace_names.insert(agent, name.to_owned());
}

/// Post `text` in `channel` as `author`; the new message's id.
pub(crate) fn say(doc: &mut Workspace, author: &str, channel: &str, text: &str) -> String {
    doc.say(&ui::tilo_resource::MemberId::person(author), channel, text)
        .unwrap()
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
pub(crate) fn messages(doc: &Workspace, channel: &str) -> Vec<Message> {
    doc.message_rows(channel)
}

/// The parts of the chat's messages a mount's store holds.
pub(crate) fn stored_messages(records: &ui::resource::ResourceStore) -> crate::messages::Messages {
    use ui::tilo_resource::Resource as _;
    crate::messages::Messages::new(
        records
            .held_parts(&crate::parts::chain())
            .into_iter()
            .filter_map(|index| {
                let part = records
                    .read(&crate::parts::part(index), |doc| {
                        crate::parts::ChatMessages::read(doc).ok()
                    })
                    .flatten()?;
                Some((index, std::rc::Rc::new(part)))
            })
            .collect(),
    )
}

/// The messages of `channel` a mount's store holds, oldest first.
pub(crate) fn messages_in(records: &ui::resource::ResourceStore, channel: &str) -> Vec<Message> {
    let chat = records
        .read(
            &ui::resource::DocumentId::package_primary(),
            crate::projection::read,
        )
        .unwrap_or_default();
    chat.message_rows(&stored_messages(records), channel)
}

/// How many top-level messages of `channel` a mount's store holds.
pub(crate) fn top_level_in(records: &ui::resource::ResourceStore, channel: &str) -> usize {
    stored_messages(records).top_level(&Id::from_raw(channel))
}

/// The parts of the chat's messages a mounted app's window holds.
pub(crate) fn mounted_messages(
    app: &mut ui::framework::testing::TestApp<crate::ChatShell>,
) -> crate::messages::Messages {
    app.with_chat(|surface| surface.held_messages())
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

/// Post `text` in `channel` through a mounted app's api, as its person.
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
    ui::tilo_resource::items::item_resource_id::<crate::parts::ChatMessages>(
        "messages",
        &Id::from_raw(id),
    )
}

/// The channels, in sidebar order.
pub(crate) fn channels(doc: &AutoCommit) -> Vec<Channel> {
    crate::projection::read(doc).channel_rows()
}

/// The people in the workspace.
pub(crate) fn users() -> Vec<User> {
    crate::projection::member_rows()
}

/// How many top-level messages `channel` has.
pub(crate) fn top_level(doc: &Workspace, channel: &str) -> usize {
    doc.messages().top_level(&Id::from_raw(channel))
}

pub(crate) fn read_mark(
    doc: &AutoCommit,
    channel: &str,
    user: &ui::tilo_resource::MemberId,
) -> crate::parts::ReadMark {
    crate::projection::read(doc).read_mark(channel, user)
}

pub(crate) fn draft_text(doc: &AutoCommit, key: &str) -> String {
    crate::projection::draft_text(doc, key)
}

/// The id of the item resource the resource publishes for `id` of `field`.
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
pub(crate) fn report_resource(id: &str, text: &str) -> ui::tilo_actions::ResourceSpec {
    ui::tilo_resource::typed_resource::<chat_model::rows::Report>(item("feedback", id), text)
        .with_action(crate::feedback::receipt_read::spec(id).into_spec())
        .with_action(crate::feedback::status_read::spec(id).into_spec())
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
    crate::chat::reply_id(
        &Id::from_raw(crate::resource::DEMO_SUMMONS),
        &crate::resource::demo_agent(),
    )
    .to_string()
}

pub(crate) fn record_work(
    summons: &str,
    work: &crate::model::AgentWork,
) -> Invoke<crate::parts::ChatMessages> {
    crate::parts::ChatMessages::RECORD_WORK.with(crate::messages::chat_messages::RecordWork {
        message: Id::from_raw(summons),
        work: work_update(work),
        say: None,
    })
}

/// The id of the call row of the voice channel `channel`.
pub(crate) fn call_row(channel: &str) -> String {
    format!("{}/call", item("channels", channel))
}

pub(crate) fn work_update(work: &crate::model::AgentWork) -> crate::chat::WorkUpdate {
    use crate::model::{AgentStepOutcome, AgentWorkState};

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
    crate::chat::WorkUpdate {
        state: state.into(),
        app,
        did,
        about,
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
                crate::chat::StepUpdate {
                    id: step.id.clone(),
                    sequence: step.sequence,
                    resource: step.resource.clone(),
                    label: step.label.clone(),
                    outcome: outcome.into(),
                    why,
                }
            })
            .collect(),
        detail: None,
        answered: work.answered.clone(),
    }
}

/// The chat a mounted app's window shows, reached as a test drives it.
pub(crate) trait MountedChat {
    /// The chat the window shows.
    fn chat(&mut self) -> std::cell::RefMut<'_, crate::ChatApp>;
    /// Visit the chat the window shows, as the next frame draws it.
    fn with_chat<T>(&mut self, visit: impl FnOnce(&mut crate::ChatApp) -> T) -> Option<T>;
}

impl MountedChat for ui::framework::testing::TestApp<crate::ChatShell> {
    fn chat(&mut self) -> std::cell::RefMut<'_, crate::ChatApp> {
        self.surface_mut::<crate::ChatWindow>().state()
    }

    fn with_chat<T>(&mut self, visit: impl FnOnce(&mut crate::ChatApp) -> T) -> Option<T> {
        self.with_surface::<crate::ChatWindow, _>(|window| visit(&mut window.state()))
    }
}
