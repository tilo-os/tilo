//! Chat as every host runs it: the [`Chat`] record, what it answers to
//! search and to a read, and the window over it.
//!
//! The record's verbs and storage are in [`crate::record`]. What the window
//! adds to the published tree is not document state: the effects it offers
//! on the items it shows (open a conversation, reveal or read a message,
//! join a voice channel's call), as live rows merged into the item
//! resources the record publishes ([`crate::effects`]).

use tilo_content::ContentStore;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, LiveRow, ResourceApp, ResourceSurface, RunMode, SeedCx,
    SurfaceCx, ViewCx,
};
use ui::tilo_resource::items::{item_of_resource, item_resource_id};
use ui::tilo_resource::{Id, Invoke, SearchEntry};
use ui::widget::Element;

use crate::app::{ChatApp, ChatRuntime};
use crate::effects::ChatEffect;
use crate::identity::LocalIdentity;
use crate::messages::chat_part;
use crate::parts::ChatPart;
pub use crate::record::Chat;
use crate::record::chat;

impl Chat {
    /// What search finds in the chat record: its workspaces, channels,
    /// categories, reviewed reports and people, each opening the item it
    /// names. The messages are the window's to offer
    /// (`ResourceSurface::search_entries`): they are kept in parts the record
    /// does not hold.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        let workspace_name = |id: &Id| {
            self.workspaces
                .get(id)
                .map_or("", |workspace| workspace.name.as_str())
        };
        let anchor = |field: &str, id: &Id| item_resource_id::<Self>(field, id);
        let workspaces = self.workspaces.iter().map(|(id, workspace)| {
            SearchEntry::new(&workspace.name, "Workspace").anchored(anchor("workspaces", id))
        });
        let channels = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.conversation_with.is_none())
            .map(|(id, channel)| {
                SearchEntry::new(
                    channel.name.clone(),
                    format!("{} {}", channel.topic, workspace_name(&channel.workspace)),
                )
                .anchored(anchor("channels", id))
            });
        let categories = self.categories.iter().map(|(id, category)| {
            SearchEntry::new(&category.name, workspace_name(&category.workspace))
                .anchored(anchor("categories", id))
        });
        let feedback = self.feedback.iter().filter_map(|(id, report)| {
            let submission = report.submission.0.as_ref()?;
            Some(
                SearchEntry::new(
                    &submission.feedback.text,
                    workspace_name(&Id::from_raw(submission.destination.room.clone())),
                )
                .anchored(anchor("feedback", id)),
            )
        });
        let people = self.people.iter().map(|person| {
            SearchEntry::new(&person.name, "Person")
                .anchored(anchor("people", &Id::from_raw(person.id.0.clone())))
        });
        workspaces
            .chain(channels)
            .chain(categories)
            .chain(feedback)
            .chain(people)
            .collect()
    }

    /// What the chat record holds, for a caller that reads it: with no item,
    /// its workspaces and their channels; with a channel, what it is about.
    /// Its messages are kept in parts beside the record: a caller reads them
    /// from the chain the window publishes, or with `read-workspace`.
    ///
    /// # Errors
    /// When `item` names nothing in the chat.
    pub fn content(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "workspaces": self.workspaces.iter().map(|(id, workspace)| serde_json::json!({
                    "id": id,
                    "name": workspace.name,
                    "channels": self.channels.iter()
                        .filter(|(_, channel)| {
                            channel.workspace == *id
                                && !channel.archived
                                && channel.conversation_with.is_none()
                        })
                        .map(|(id, channel)| serde_json::json!({
                            "id": id,
                            "name": channel.name,
                            "topic": channel.topic,
                            "voice": channel.kind == crate::model::ChannelKind::Voice,
                        }))
                        .collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            }));
        };
        let (field, id) = item_of_resource::<Self>(item.as_str())
            .map_or_else(|| ("", item.clone()), |(field, id)| (field, id));
        let channel = self
            .channels
            .get(&id)
            .filter(|_| field != "messages")
            .ok_or_else(|| format!("there is no `{item}` in this chat"))?;
        Ok(serde_json::json!({
            "id": id,
            "name": channel.name,
            "topic": channel.topic,
        }))
    }
}

/// Chat as every host runs and mounts it: the chat widget is the surface
/// over [`Chat`], kept by the room as its own record.
pub struct ChatShell;

/// Every named scenario `--shots --scenario NAME` captures.
pub(crate) const SCENARIOS: &[&str] = &[
    "agent-streaming",
    "agent-failed",
    "agent-reasoning",
    "agent-long-thread",
    "TaskGoalBeforeSetup",
    "TaskGoalCompact",
    "TaskTreeCompact",
    "TaskGoalFloating",
    "QueuedTask",
    "Subtask",
    "RunningSteering",
    "TaskConversationFloating",
    "TaskAnswer",
    "TaskAnswerFloating",
    "agent-work",
    "bridged",
    "agent-task",
    "agent-task-compact",
    "agent-task-queued",
    "agent-task-queued-compact",
    "narrow",
    "history",
    "compact",
    "starter",
    "people",
    "namesakes",
    "thread-narrow",
    "collaboration",
    "live-context",
    "context",
    "msgmenu",
    "attach",
    "attach-loading",
    "attach-error",
    "edit",
    "composer",
    "call",
    "share",
    "floating-call-attached",
    "floating-call",
    "floating-call-hover",
    "floating-call-returning",
    "floating-call-returned",
];

/// The draft the `composer` scenario shows mid-compose.
const COMPOSER_DRAFT: &str = "Shipping the composer on the shared TextInput — real caret, \
     selection, and Ctrl+C/V now.\nEnter sends, Shift+Enter for a new line.\nPlease check teh adress";

impl ResourceApp for ChatShell {
    type Resource = Chat;
    const ID: &'static str = "so.tilo.chat";
    const TITLE: &'static str = "Chat";
    const ICON: Option<&'static str> = Some("chat");
    const ROOM_RECORD: bool = true;

    /// The chat's messages, kept in parts beside the room's chat record.
    fn records(records: &mut ui::framework::Records<'_>) {
        records.chain::<crate::parts::ChatPart>();
    }

    fn view(cx: &mut ViewCx<Chat>) -> Element {
        crate::i18n::ensure();
        let me = cx.me().unwrap_or_default().to_owned();
        let live = cx.mode().is_live();
        // Native path access controls attachment import. A live browser
        // independently discovers its host's voice service through media replies.
        let native = live && crate::app::native_file_access();
        let content = cx.content().unwrap_or_else(ContentStore::memory);
        // A capture shows the app as a device with its services draws it;
        // nothing it stages reaches them.
        let runtime = if native || cx.mode() == RunMode::Capture {
            ChatRuntime::Native
        } else {
            ChatRuntime::Unavailable
        };
        let identity = LocalIdentity::for_participant(&me);
        let models = if live {
            // The host supplies its default capability before any explicit
            // mount-local loans. The most recent loan is authoritative, even
            // when it deliberately withdraws model access.
            let mut models = None;
            while let Some(loan) = cx.take_lent::<ui::models::Models>() {
                models = Some(loan);
            }
            crate::standalone_models().or(models).unwrap_or_default()
        } else {
            ui::models::Models::default()
        };
        // What the person is writing is theirs: kept in their own record.
        let drafts = cx.person::<crate::drafts::ChatDrafts>();
        let app = ChatApp::new(identity, runtime, content)
            .with_models(models)
            .with_drafts(drafts);
        // A search hit, or an agent's `open`, shows the item it names.
        cx.on_open(app.opener());
        cx.surface(if live && ui::local_identity::bound().is_some() {
            app.announcing()
        } else {
            app
        })
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// `composer`: a multi-line draft typed in the composer, the person's
    /// own until sent.
    fn scenario_input(
        name: Option<&str>,
        input: &mut ui::headless::FrameHarness,
    ) -> Result<(), String> {
        if name == Some("composer") {
            for (index, line) in COMPOSER_DRAFT.lines().enumerate() {
                if index > 0 {
                    input.set_modifiers(&["shift".to_owned()]);
                    input.key("Enter");
                    input.set_modifiers(&[]);
                }
                input.type_text(line);
            }
            input.relayout();
        }
        Ok(())
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some(
                "agent-task-compact"
                | "agent-task-queued-compact"
                | "TaskGoalCompact"
                | "TaskTreeCompact",
            ) => (420, 680),
            _ => (1280, 800),
        }
    }

    /// The agent scenarios are the demo workspace and what the agent did
    /// in it, each change written by whoever made it; their messages are
    /// staged in the chat's parts ([`part_scenario`]).
    fn fixture(fixture: &mut ui::framework::Fixture<Chat>) {
        if matches!(
            fixture.scenario(),
            Some("agent-streaming" | "agent-failed" | "agent-reasoning" | "agent-long-thread")
        ) {
            for invoke in demo_invokes() {
                fixture.version(crate::model::ME, crate::demo::NOW * 1000, invoke);
            }
        }
        if fixture.scenario() == Some("agent-work") {
            for (author, invoke) in agent_work_scenario() {
                fixture.version(author, crate::demo::NOW * 1000, invoke);
            }
        }
    }

    /// The first-run workspace for `starter`, the demo conversation for
    /// every other scenario, both read by the capture's own person.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Chat>> {
        match name {
            Some(
                "agent-work" | "agent-streaming" | "agent-failed" | "agent-reasoning"
                | "agent-long-thread",
            ) => Vec::new(),
            Some("starter") => {
                let you = LocalIdentity::demo();
                vec![Chat::BEGIN_CHAT.with(chat::BeginChat {
                    workspace: None,
                    workspace_name: None,
                    name: you.name,
                    initials: you.initials,
                    color: u32::try_from(you.color_idx).unwrap_or_default(),
                })]
            }
            _ => demo_invokes(),
        }
    }

    /// A new record's first workspace, owned by its creator: the one the
    /// host admitted it for, or a starter workspace. A capture's record is
    /// its scenario's instead.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Chat>> {
        if cx.host().is_some_and(|host| host.capture().is_some()) {
            return Vec::new();
        }
        let creator = LocalIdentity::for_participant(cx.me());
        let (workspace, workspace_name) = cx
            .creation_workspace()
            .map_or((None, None), |(workspace, name)| {
                (Some(workspace.to_owned()), Some(name.to_owned()))
            });
        vec![Chat::BEGIN_CHAT.with(chat::BeginChat {
            workspace,
            workspace_name,
            name: creator.name,
            initials: creator.initials,
            color: u32::try_from(creator.color_idx).unwrap_or_default(),
        })]
    }
}

/// The messages a capture of `scenario` shows, each with who wrote it: the
/// demo conversation and what the scenario adds to it, staged in the chat's
/// parts once the window holds its chain ([`ChatApp::stage_parts`]). The
/// first-run workspace has none.
pub(crate) fn part_scenario(scenario: Option<&str>) -> Vec<(&'static str, Invoke<ChatPart>)> {
    let me = crate::model::ME;
    if scenario == Some("starter") {
        return Vec::new();
    }
    let mut staged: Vec<(&'static str, Invoke<ChatPart>)> = demo_part_invokes()
        .into_iter()
        .map(|invoke| (me, invoke))
        .collect();
    match scenario {
        Some("bridged") => staged.extend(Invoke::edit(&bridged_post()).map(|invoke| (me, invoke))),
        Some(
            scenario @ ("agent-streaming" | "agent-failed" | "agent-reasoning"
            | "agent-long-thread"),
        ) => staged.extend(
            provider_stream_scenario(scenario)
                .into_iter()
                .map(|invoke| (me, invoke)),
        ),
        Some("agent-work") => staged.extend(agent_work_part_steps()),
        _ => {}
    }
    staged
}

/// Provider output fixtures use the same durable action as a live host stream.
pub(crate) fn provider_stream_scenario(scenario: &str) -> Vec<Invoke<ChatPart>> {
    use crate::agent_stream::{AgentFinish, AgentOutput, AgentStreamEvent};
    let mut events = vec![
        AgentOutput::Progress { label: "Reading the selected document".into() },
        AgentOutput::ReasoningDelta { text: "The provider shared this summary: compare the selected sections before proposing changes.".into() },
        AgentOutput::TextDelta { text: "The document describes two approaches. The first preserves the existing layout; the second changes how the sections are grouped.".into() },
    ];
    if scenario == "agent-long-thread" {
        events.push(AgentOutput::ReasoningDelta {
            text: "\nCompare the evidence, preserve the source, and check each conclusion before presenting the result.\n".repeat(40),
        });
        events.push(AgentOutput::TextDelta {
            text: "\n\nThe complete answer includes the evidence and the practical next step. Keep the original document and review changes before applying them.\n".repeat(30),
        });
        events.push(AgentOutput::Finished {
            outcome: AgentFinish::Complete,
            detail: "All sections reviewed.".into(),
        });
    }
    if scenario == "agent-failed" {
        events.push(AgentOutput::Finished {
            outcome: AgentFinish::Failed,
            detail: "Connection interrupted. Check connectivity and provider settings.".into(),
        });
    }
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            ChatPart::STREAM_OUTPUT.with(chat_part::StreamOutput {
                channel: Id::from_raw("c_crit"),
                reply: "shot_agent_stream".into(),
                name: Some(crate::i18n::t("chat-model-author")),
                event: serde_json::to_value(AgentStreamEvent {
                    run: "shot_run".into(),
                    sequence: index as u64 + 1,
                    event,
                })
                .unwrap_or_default(),
            })
        })
        .collect()
}

/// The demo workspace's chat record, read by [`crate::model::ME`], as a
/// scenario writes it.
fn demo_invokes() -> Vec<Invoke<Chat>> {
    crate::demo::edits(&crate::demo::demo(crate::model::ME))
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect()
}

/// The demo conversation, read by [`crate::model::ME`], as a scenario
/// writes it into the first part of the chat's messages.
pub(crate) fn demo_part_invokes() -> Vec<Invoke<ChatPart>> {
    crate::demo::part_edits(&crate::demo::demo_part(crate::model::ME))
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect()
}

/// The bridge the `bridged` scenario's post is by.
pub(crate) const DEMO_BRIDGE: &str = "bot:5d0c7a3e-2b41-4f6a-9e18-c3a7b2d4e6f1";

/// `bridged`: a bridge's post of a line someone wrote on Discord, shown
/// under their name and picture while it stays the bridge's.
fn bridged_post() -> ui::tilo_resource::Edit<ChatPart> {
    let post = crate::record::Message {
        channel: Id::from_raw("c_crit"),
        author: ui::tilo_resource::Atom(DEMO_BRIDGE.to_owned()),
        ts: crate::demo::NOW - 60,
        text: "Joining from Discord: the warmer palette reads much better on my phone.".into(),
        appearance: ui::tilo_resource::Json(Some(tilo_actions::Appearance {
            name: "Bo".into(),
            avatar: Some("tilo-demo://avatar-bo".into()),
            persona: Some(tilo_actions::Persona {
                service: "discord".into(),
                account: "80351110224678912".into(),
            }),
        })),
        ..crate::record::Message::default()
    };
    let at = crate::demo::demo_part(crate::model::ME).messages.len();
    ChatPart::paths()
        .messages
        .insert_as(&Id::from_raw("m_bridged"), at, &post)
}

/// The agent the `agent-work` scenario summons.
pub(crate) const DEMO_AGENT: &str = "agent-task:11111111-1111-4111-8111-111111111111";
/// The message summoning it.
pub(crate) const DEMO_SUMMONS: &str = "m_agent_request";

/// `agent-work`'s chat record: the demo workspace and the agent introducing
/// itself and being named, each by its author. Its messages are
/// [`agent_work_part_steps`].
pub(crate) fn agent_work_scenario() -> Vec<(&'static str, Invoke<Chat>)> {
    let me = crate::model::ME;
    demo_invokes()
        .into_iter()
        .map(|invoke| (me, invoke))
        .chain(agent_work_steps())
        .collect()
}

/// On the demo workspace: the agent introducing itself and being named,
/// each by its author.
pub(crate) fn agent_work_steps() -> Vec<(&'static str, Invoke<Chat>)> {
    let me = crate::model::ME;
    vec![
        (
            DEMO_AGENT,
            Chat::INTRODUCE.with(chat::Introduce {
                name: "Tide".into(),
                initials: "TI".into(),
                color: 6,
            }),
        ),
        (
            me,
            Chat::NAME_AGENT.with(chat::NameAgent {
                participant: DEMO_AGENT.into(),
                name: "Tide".into(),
            }),
        ),
    ]
}

/// On the demo conversation: a person summoning the agent, and the agent's
/// record of the work it took on, each by its author.
pub(crate) fn agent_work_part_steps() -> Vec<(&'static str, Invoke<ChatPart>)> {
    let me = crate::model::ME;
    let summons = crate::record::Message {
        channel: Id::from_raw("c_crit"),
        author: ui::tilo_resource::Atom(me.to_owned()),
        ts: crate::demo::NOW - 120,
        text: "@Tide add the revenue curve to Calculator and keep the current bounds.".into(),
        mentions: ui::tilo_resource::Json(vec![crate::mention::Mention {
            user: DEMO_AGENT.into(),
            start: 0,
            len: "@Tide".len(),
            is_agent: true,
        }]),
        ..crate::record::Message::default()
    };
    let at = crate::demo::demo_part(me).messages.len();
    let ask = ChatPart::paths()
        .messages
        .insert_as(&Id::from_raw(DEMO_SUMMONS), at, &summons);
    let mut steps: Vec<(&'static str, Invoke<ChatPart>)> = Invoke::edit(&ask)
        .into_iter()
        .map(|invoke| (me, invoke))
        .collect();
    steps.push((DEMO_AGENT, agent_work(&Id::from_raw(DEMO_SUMMONS))));
    steps
}

/// The demo agent's record of the work `summons` asked of it.
pub(crate) fn agent_work(summons: &Id) -> Invoke<ChatPart> {
    ChatPart::RECORD_WORK.with(chat_part::RecordWork {
        message: summons.clone(),
        work: crate::record::WorkUpdate {
            state: "doing".into(),
            app: None,
            did: Some("Add function".into()),
            about: Some("Revenue curve, preserving the current graph bounds".into()),
            route: Some("model".into()),
            why: Some(
                "Calculator offers the requested graph operation and no bounds change is needed."
                    .into(),
            ),
            trace: vec![crate::record::StepUpdate {
                id: "00000000-0000-4000-8000-000000000001".into(),
                sequence: 0,
                resource: "calculator.workspace".into(),
                label: "Add function".into(),
                outcome: "asked".into(),
                why: None,
            }],
        },
        say: None,
    })
}

impl ResourceSurface<Chat, ChatEffect> for ChatApp {
    fn present(&mut self, _state: &std::rc::Rc<Chat>) {
        // The surface reads the document it syncs with, and reloads when its
        // heads move.
    }

    fn attach(&mut self, cx: SurfaceCx<Chat>) {
        self.attach_framework(cx.dispatcher());
        if let Some(chain) = cx.chain::<crate::parts::ChatPart>() {
            self.attach_chain(chain);
        }
        if cx.mode().is_live() {
            self.follow_preferences(cx.settings());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_parts(scenario);
        self.stage_scenario(scenario.unwrap_or_default());
    }

    fn effect(&mut self, effect: ChatEffect, cx: &mut EffectCx<Chat, Self>) -> EffectAnswer {
        match &effect {
            ChatEffect::AdoptPrivateDraft { token } => {
                let invocation = cx
                    .invocation()
                    .ok_or("private draft requires original person invocation")?;
                return ui::person_chat::with_invocation(Some(invocation), || {
                    self.adopt_private_draft(token, invocation)
                });
            }
            ChatEffect::OpenDirectConversation { .. }
            | ChatEffect::SendDirectMessage { .. }
            | ChatEffect::ReadDirectConversation { .. } => {
                return ui::person_chat::with_invocation(cx.invocation(), || {
                    self.answer_effect(effect)
                });
            }
            _ => {}
        }
        if let ChatEffect::ReadWorkspace {
            workspace,
            message,
            channel,
        } = &effect
        {
            let chat = cx.live().get();
            return Ok(ui::framework::Effected::value(
                crate::effects::read_workspace(
                    &chat,
                    &self.held_messages(),
                    workspace,
                    message.as_deref(),
                    channel.as_deref(),
                ),
            ));
        }
        if let ChatEffect::ReadWorkspace {
            workspace,
            message,
            channel,
        } = &effect
        {
            let chat = cx.live().get();
            return Ok(ui::framework::Effected::value(
                crate::effects::read_workspace(
                    &chat,
                    &self.held_messages(),
                    workspace,
                    message.as_deref(),
                    channel.as_deref(),
                ),
            ));
        }
        if let Some(url) = crate::external::browser_destination(&effect)? {
            return Ok(cx.thread(
                move |_| ui::widget::try_open_external_url(&url).map_err(|error| error.to_string()),
                |_: &mut Self, opened, _| opened.map(|()| ui::framework::Effected::done()),
            ));
        }
        self.answer_effect(effect)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// The effects offered on the workspaces, channels, categories,
    /// messages, reports and people the window shows: merged into the item
    /// resources the record publishes.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.published_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.published_row(id)
    }

    /// The messages this window holds, across the parts of the chat it
    /// has: each hit reveals the message it names.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.search_messages
            .iter()
            .map(|message| {
                let author = self
                    .search_users
                    .iter()
                    .find(|user| user.id == message.author)
                    .map_or(message.author.as_str(), |user| user.name.as_str());
                SearchEntry::new(author, &message.text).anchored(item_resource_id::<Chat>(
                    "messages",
                    &Id::from_raw(message.id.clone()),
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
