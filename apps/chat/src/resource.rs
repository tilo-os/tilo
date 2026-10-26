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
    /// What search finds in the chat record: the room's channels,
    /// categories, reviewed reports and people, each opening the item it
    /// names. The messages are the window's to offer
    /// (`ResourceSurface::search_entries`): they are kept in parts the record
    /// does not hold.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        let anchor = |field: &str, id: &Id| item_resource_id::<Self>(field, id);
        let channels = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.conversation_with.is_none())
            .map(|(id, channel)| {
                SearchEntry::new(channel.name.clone(), channel.topic.clone())
                    .anchored(anchor("channels", id))
            });
        let categories = self.categories.iter().map(|(id, category)| {
            SearchEntry::new(&category.name, "Category").anchored(anchor("categories", id))
        });
        // A report is found by what its message says: the summary, and the
        // picks of one filed without words.
        let feedback = self.feedback.iter().filter_map(|(id, report)| {
            let message = crate::feedback::message::message(report)?;
            Some(SearchEntry::new(&message.text, "Feedback").anchored(anchor("feedback", id)))
        });
        let people = self.people.iter().map(|person| {
            SearchEntry::new(&person.name, "Person")
                .anchored(anchor("people", &Id::from_raw(person.id.0.clone())))
        });
        channels
            .chain(categories)
            .chain(feedback)
            .chain(people)
            .collect()
    }

    /// What the chat record holds, for a caller that reads it: with no item,
    /// the room's channels; with a channel, what it is about. Its messages
    /// are kept in parts beside the record: a caller reads them from the
    /// chain the window publishes, or with `read-room`.
    pub fn content(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "channels": self.channels.iter()
                    .filter(|(_, channel)| {
                        !channel.archived && channel.conversation_with.is_none()
                    })
                    .map(|(id, channel)| serde_json::json!({
                        "id": id,
                        "name": channel.name,
                        "topic": channel.topic,
                        "voice": channel.kind == crate::model::ChannelKind::Voice,
                    }))
                    .collect::<Vec<_>>(),
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
    "agent-work",
    "bridged",
    "narrow",
    "history",
    "compact",
    "starter",
    "people",
    "namesakes",
    "thread-narrow",
    "collaboration",
    "lounge",
    "lounge-call",
    "lounge-video",
    "lounge-share",
    "lounge-bridged",
    "live-context",
    "context",
    "msgmenu",
    "attach",
    "attach-loading",
    "attach-error",
    "attach-upload",
    "edit",
    "composer",
    "call",
    "share",
    "floating-call-attached",
    "floating-call",
    "floating-call-hover",
    "floating-call-returning",
    "floating-call-returned",
    "channels",
    "mention",
    "new-channel",
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
        // The workspace's emoji are Chat's: every app of the room opens
        // them, and their changes are decided against Chat's surface.
        records.workspace::<ui::tilo_workspace_emoji::WorkspaceEmoji>();
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
        // The shell keeps the room's channels as its sidebar, their width,
        // their drawer in a narrow window and their keys; the conversation
        // is the window's point, a reading column beside them.
        let places = ui::widget::NavFeed::new();
        let shell = ui::widget::NavShell::new()
            .with_id("chat-shell")
            .content_kind(ui::widget::ContentKind::Reading)
            .with_window_toolbar();
        let app = app.in_shell(&places, shell.window_bar_link());
        let app = if live && ui::local_identity::bound().is_some() {
            app.announcing()
        } else {
            app
        };
        Box::new(
            shell
                .sidebar(crate::sidebar::sidebar(&places))
                .content(cx.surface(app)),
        )
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// `composer`: a multi-line draft typed in the composer, the person's
    /// own until sent. `mention`: someone typing `@a` in the composer, the
    /// editor's `@` list open under it.
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
        if name == Some("mention") {
            input.type_text("Could you take a look, @a");
            for _ in 0..3 {
                input.relayout();
            }
        }
        Ok(())
    }

    /// Every scenario but `starter` starts from the demo workspace, read by
    /// the capture's own person: its people and its channels under the
    /// fixed ids the scenarios name, which only the moderators' actions
    /// make, minting their own. `agent-work` adds the agent introducing
    /// itself and being named, each change written by whoever made it.
    /// Their messages are staged in the chat's parts (`part_scenario`).
    fn fixture(fixture: &mut ui::framework::Fixture<Chat>) {
        if fixture.scenario() == Some("starter") {
            return;
        }
        fixture.start(crate::demo::demo(crate::model::ME));
        if fixture.scenario() == Some("agent-work") {
            for (author, invoke) in agent_work_steps() {
                fixture.version(author, crate::demo::NOW * 1000, invoke);
            }
        }
    }

    /// The first-run workspace for `starter`, read by the capture's own
    /// person; every other scenario starts from its [`Self::fixture`].
    fn scenario(name: Option<&str>) -> Vec<Invoke<Chat>> {
        if name != Some("starter") {
            return Vec::new();
        }
        let you = LocalIdentity::demo();
        vec![Chat::BEGIN_CHAT.with(chat::BeginChat {
            name: you.name,
            initials: you.initials,
            color: u32::try_from(you.color_idx).unwrap_or_default(),
        })]
    }

    /// A new record's creator, and the room's first channels, `#chat` and
    /// `#voice`, when it has none: under fixed ids and with the same values
    /// on every device, so concurrent first opens on several devices
    /// converge on exactly those two. A capture's record is its scenario's
    /// instead.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Chat>> {
        if cx.host().is_some_and(|host| host.capture().is_some()) {
            return Vec::new();
        }
        let creator = LocalIdentity::for_participant(cx.me());
        vec![Chat::BEGIN_CHAT.with(chat::BeginChat {
            name: creator.name,
            initials: creator.initials,
            color: u32::try_from(creator.color_idx).unwrap_or_default(),
        })]
    }
}

/// The messages a capture of `scenario` shows, staged in the chat's parts
/// once the window holds its chain ([`ChatApp::stage_parts`]): the first
/// part's messages as it starts, by several people under the fixed ids the
/// scenario names, which no one person's post makes; then what is done
/// with them, each by who did it. The first-run workspace has none.
pub(crate) fn part_scenario(
    scenario: Option<&str>,
) -> (Option<ChatPart>, Vec<(&'static str, Invoke<ChatPart>)>) {
    let me = crate::model::ME;
    if scenario == Some("starter") {
        return (None, Vec::new());
    }
    let mut start = crate::demo::demo_part(me);
    let mut steps = Vec::new();
    match scenario {
        Some("bridged") => {
            let (id, message) = bridged_post();
            start.messages.push(id, message);
        }
        Some("channels") => {
            let (id, message) = mention_post();
            start.messages.push(id, message);
        }
        Some(
            scenario @ ("agent-streaming" | "agent-failed" | "agent-reasoning"
            | "agent-long-thread"),
        ) => steps.extend(
            provider_stream_scenario(scenario)
                .into_iter()
                .map(|invoke| (me, invoke)),
        ),
        Some("agent-work") => {
            let (id, message) = agent_summons();
            start.messages.push(id, message);
            steps.extend(agent_work_part_steps());
        }
        _ => {}
    }
    (Some(start), steps)
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

/// `channels`: a message elsewhere that mentions the reader, which the
/// sidebar badges on its channel.
fn mention_post() -> (Id, crate::record::Message) {
    let text = "@You could you check the hover states on the new sidebar before standup?";
    let post = crate::record::Message {
        channel: Id::from_raw("c_frontend"),
        author: ui::tilo_resource::Atom("u_aria".to_owned()),
        ts: crate::demo::NOW - 30,
        text: text.into(),
        mentions: vec![crate::mention::Mention {
            user: crate::model::ME.into(),
            start: 0,
            len: "@You".len(),
            is_agent: false,
        }],
        ..crate::record::Message::default()
    };
    (Id::from_raw("m_mention"), post)
}

/// The bridge the `bridged` scenario's post is by.
pub(crate) const DEMO_BRIDGE: &str = "bot:5d0c7a3e-2b41-4f6a-9e18-c3a7b2d4e6f1";

/// `bridged`: a bridge's post of a line someone wrote on Discord, shown
/// under their name and picture while it stays the bridge's.
fn bridged_post() -> (Id, crate::record::Message) {
    let post = crate::record::Message {
        channel: Id::from_raw("c_crit"),
        author: ui::tilo_resource::Atom(DEMO_BRIDGE.to_owned()),
        ts: crate::demo::NOW - 60,
        text: "Joining from Discord: the warmer palette reads much better on my phone.".into(),
        appearance: Some(crate::record::ShownAs {
            name: "Bo".into(),
            avatar: Some("tilo-demo://avatar-bo".into()),
            persona: Some(crate::record::ShownPersona {
                service: "discord".into(),
                account: "80351110224678912".into(),
                linked: None,
            }),
        }),
        ..crate::record::Message::default()
    };
    (Id::from_raw("m_bridged"), post)
}

/// The agent the `agent-work` scenario summons.
pub(crate) const DEMO_AGENT: &str = "agent-task:11111111-1111-4111-8111-111111111111";
/// The message summoning it.
pub(crate) const DEMO_SUMMONS: &str = "m_agent_request";

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

/// In the demo conversation: the reader summoning the agent, under the id
/// [`DEMO_SUMMONS`] the agent's record of its work names.
pub(crate) fn agent_summons() -> (Id, crate::record::Message) {
    let summons = crate::record::Message {
        channel: Id::from_raw("c_crit"),
        author: ui::tilo_resource::Atom(crate::model::ME.to_owned()),
        ts: crate::demo::NOW - 120,
        text: "@Tide add the revenue curve to Calculator and keep the current bounds.".into(),
        mentions: vec![crate::mention::Mention {
            user: DEMO_AGENT.into(),
            start: 0,
            len: "@Tide".len(),
            is_agent: true,
        }],
        ..crate::record::Message::default()
    };
    (Id::from_raw(DEMO_SUMMONS), summons)
}

/// On the demo conversation holding [`agent_summons`]: the agent's record
/// of the work it took on, by the agent.
pub(crate) fn agent_work_part_steps() -> Vec<(&'static str, Invoke<ChatPart>)> {
    vec![(DEMO_AGENT, agent_work(&Id::from_raw(DEMO_SUMMONS)))]
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
            ChatEffect::OpenDirectConversation { .. } => {
                return ui::person_chat::with_invocation(cx.invocation(), || {
                    self.answer_effect(effect)
                });
            }
            // The room answers these: at once from the native host, later in
            // a page. Either way the effect answers when it has.
            ChatEffect::SendDirectMessage { public_key, text } => {
                let reply = ui::person_chat::with_invocation(cx.invocation(), || {
                    self.send_person_message(public_key, text)
                })?;
                let public_key = public_key.clone();
                return Ok(cx.spawn(
                    |_| reply,
                    move |chat: &mut Self, answer, _| chat.sent_person_answer(&public_key, answer),
                ));
            }
            ChatEffect::ReadDirectConversation { public_key } => {
                let reply = ui::person_chat::with_invocation(cx.invocation(), || {
                    self.read_person_conversation(public_key)
                })?;
                let public_key = public_key.clone();
                return Ok(cx.spawn(
                    |_| reply,
                    move |chat: &mut Self, answer, _| chat.read_person_answer(&public_key, answer),
                ));
            }
            _ => {}
        }
        if let ChatEffect::ReadMessages {
            channel,
            page_size,
            cursor,
        } = &effect
        {
            return crate::effects::read_messages(
                &self.held_messages(),
                (channel, &|id| self.display_name(id)),
                *page_size,
                cursor.as_deref(),
            )
            .map(ui::framework::Effected::value);
        }
        if let ChatEffect::ExportContent { channel, format } = &effect {
            return self.export_transcript(&cx.live().get(), channel, format.as_deref());
        }
        if let ChatEffect::ReadRoom { message, channel } = &effect {
            let chat = cx.live().get();
            return Ok(ui::framework::Effected::value(crate::effects::read_room(
                &chat,
                &self.held_messages(),
                message.as_deref(),
                channel.as_deref(),
            )));
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

    /// The effects offered on the channels, categories,
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

impl ChatApp {
    /// Keep `channel`'s transcript as content ([`ChatEffect::ExportContent`]).
    fn export_transcript(&self, chat: &Chat, channel: &str, format: Option<&str>) -> EffectAnswer {
        let id = Id::from_raw(channel);
        let found = chat
            .channels
            .get(&id)
            .filter(|found| found.conversation_with.is_none())
            .ok_or_else(|| format!("there is no channel `{channel}` in this chat"))?;
        let held = self.held_messages();
        let mut messages: Vec<_> = held
            .iter()
            .filter(|(_, message)| message.channel == id)
            .collect();
        // Oldest first; posts of one second keep the chain's order.
        messages.sort_by_key(|(_, message)| message.ts);
        let transcript = crate::transcript::Channel {
            id: channel,
            name: &found.name,
            messages,
        };
        let bytes = crate::transcript::transcript(&transcript, format, &|author| {
            self.display_name(author)
        })?;
        let hash = self
            .content
            .put(&bytes)
            .map_err(|error| format!("the transcript could not be kept: {error}"))?;
        Ok(crate::transcript::exported(
            &hash,
            format,
            transcript.messages.len(),
            bytes.len(),
        ))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// The walk's slices: a drive of one scenario walks dozens of actions,
    /// so each slice is its own test and they run side by side.
    const SLICES: usize = 6;

    /// Every action the app publishes is reachable by address in a drive of
    /// every `SLICES`th scenario from `slice`: none is advertised yet
    /// handled only by a click. The scenarios stage fixtures; the app
    /// someone opens acts on this machine, so it is not walked.
    fn walk(slice: usize) {
        for scenario in super::ChatShell::scenarios()
            .iter()
            .skip(slice)
            .step_by(SLICES)
        {
            let mut app = TestApp::<super::ChatShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_0() {
        walk(0);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_1() {
        walk(1);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_2() {
        walk(2);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_3() {
        walk(3);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_4() {
        walk(4);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_5() {
        walk(5);
    }
}
