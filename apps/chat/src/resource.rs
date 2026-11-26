//! Chat as every host runs it: the [`Chat`] resource, what it answers to
//! search and to a read, and the window over it.
//!
//! The resource's verbs and storage are in [`crate::chat`]. What the window
//! adds to the published tree is not document state: the effects it offers
//! on the items it shows (open a conversation, reveal or read a message,
//! join a voice channel's call), as live rows merged into the item
//! resources the resource publishes ([`crate::effects`]).

use crate::projection::ChatProjection as _;
use std::cell::RefCell;
use std::rc::Rc;

use tilo_content::ContentStore;
use ui::framework::{EffectAnswer, EffectCx, Publishes, ResourceApp, RunMode, SeedCx, ViewCx};
use ui::tilo_resource::{Id, Invoke};
use ui::widget::Element;

use crate::app::{ChatApp, ChatRuntime, ChatWindow, SharedChat};
pub use crate::chat::Chat;
use crate::chat::chat;
use crate::effects::ChatEffect;
use crate::identity::LocalIdentity;
#[cfg(feature = "samples")]
use crate::messages::chat_messages;
#[cfg(feature = "samples")]
use crate::parts::ChatMessages;

/// Chat as every host runs and mounts it: the chat widget is the window
/// over [`Chat`], kept by the workspace as its own document.
pub struct ChatShell;

pub use chat_model::chat::conversation_entry;

/// Every named scenario `--shots --scenario NAME` captures.
#[cfg(feature = "samples")]
pub(crate) const SCENARIOS: &[&str] = &[
    "agent-streaming",
    "agent-failed",
    "agent-reasoning",
    "agent-long-thread",
    "long-thread",
    "agent-work",
    "agent-stopped",
    "agent-went-on",
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
    "live-context",
    "context",
    "msgmenu",
    "msgbar",
    "msgbar-shift",
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
    "floating-call-crowd",
    "floating-call-muted",
    "floating-call-hover",
    "floating-call-returning",
    "floating-call-returned",
    "channels",
    "mention",
    "new-channel",
];

/// The draft the `composer` scenario shows mid-compose.
#[cfg(feature = "samples")]
const COMPOSER_DRAFT: &str = "Shipping the composer on the shared TextInput — real caret, \
     selection, and Ctrl+C/V now.\nEnter sends, Shift+Enter for a new line.\nPlease check teh adress";

impl ResourceApp for ChatShell {
    type Resource = Chat;
    const ID: &'static str = ui::apps::CHAT;
    const TITLE: &'static str = "Chat";
    const TITLE_KEY: &'static str = "chat-app-title";
    const ICON: Option<&'static str> = Some("chat");
    const WORKSPACE_RESOURCE: bool = true;

    /// The chat's messages, kept in parts beside the workspace's chat resource.
    fn resources(resources: &mut ui::framework::Resources<'_>) {
        resources.chain::<crate::parts::ChatMessages>();
        resources.workspace_record::<crate::drafts::ChatDrafts>(crate::drafts::DOCUMENT);
        resources.shared_workspace::<chat_model::Moderation>();
        // The workspace's emoji is Chat's: every app of the workspace opens it,
        // and its changes are decided against Chat's surface.
        resources.shared_workspace::<ui::tilo_workspace_emoji::WorkspaceEmoji>();
        resources.shared_workspace::<crate::skills::Skills>();
    }

    fn view(cx: &mut ViewCx<Chat>) -> Element {
        crate::i18n::ensure();
        // A view with no reader reads as a person with no id.
        let me = cx
            .me()
            .cloned()
            .unwrap_or_else(|| ui::tilo_resource::MemberId::person(String::new()));
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
        let identity = LocalIdentity::for_member(&me);
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
        let drafts =
            cx.shared_resource::<crate::drafts::ChatDrafts>(Self::ID, crate::drafts::DOCUMENT);
        let app = ChatApp::new(identity, runtime, content)
            .with_models(models)
            .with_model_choices(live)
            .with_drafts(drafts)
            .with_reports(cx.resource_opener::<chat_model::Moderation>(Self::ID))
            // The workspace's names for its agents, read from the list every app of the
            // workspace opens: an agent has no presence here to be named by.
            .with_agents(cx.resource_opener(ui::tilo_workspace_agent::OWNER))
            // The window's actions: its composer's send button is where Chat's
            // `send` lands, the action an agent's send runs too.
            .with_send_action(&cx.actions());
        // A search hit, or an agent's `open`, shows the item it names.
        cx.on_open(app.opener());
        // The shell keeps the workspace's channels as its sidebar, their width,
        // their drawer in a narrow window and their keys; the conversation
        // is the window's point, a reading column beside them.
        let places = ui::widget::NavFeed::new();
        let shell = ui::widget::NavShell::new()
            .with_id("chat-shell")
            .content_kind(ui::widget::ContentKind::Reading)
            .with_window_toolbar();
        let mut app = app.in_shell(&places, shell.window_bar_link());
        // Where an answer to a question says it was pressed.
        app.device = cx.device().map(str::to_owned);
        app.attach_framework(cx.dispatcher());
        if let Some(chain) = cx.chain::<crate::parts::ChatMessages>() {
            app.attach_chain(chain);
        }
        if live {
            app.follow_preferences(cx.settings());
        }
        app.jobs = Some(cx.window_jobs::<ChatWindow>());
        let chat: SharedChat = Rc::new(RefCell::new(app));
        let window = ChatWindow::new(Rc::clone(&chat));
        #[cfg(feature = "samples")]
        cx.on_stage(window.stager());
        let answering = Rc::clone(&chat);
        let statuses = Rc::clone(&chat);
        cx.effects(move |effect: ChatEffect, cx| answer(&answering, effect, cx))
            .status(move || statuses.borrow().effect_statuses());
        let rows = Rc::clone(&chat);
        let row = Rc::clone(&chat);
        let statuses = Rc::clone(&chat);
        // What the window lists that the resource keeps no row for: a direct conversation a send
        // opened, an agent's among them, named by what the workspace calls its agents. The
        // sidebar lists the same conversations, so search and the sidebar never name one
        // differently.
        let unrecorded = Rc::clone(&chat);
        let window = cx.publishing(
            Publishes::new()
                // The effects offered on the channels, categories, messages,
                // reports and people the window shows: merged into the item
                // resources the resource publishes.
                .live_rows(move || rows.borrow().published_rows())
                .live_row(move |id| row.borrow().published_row(id))
                // A call's controls say they need the call it belongs to.
                .row_status(move |id| statuses.borrow().row_status(id))
                // The reads a conversation and a report answer from the
                // chat's committed state.
                .item_reads(ChatApp::item_reads)
                .search_entries(move || unrecorded.borrow().unrecorded_conversation_entries()),
            Box::new(window),
        );
        let window = conversation_setup(cx, &chat, window);
        Box::new(
            shell
                .sidebar(crate::sidebar::sidebar(
                    &places,
                    cx.actions().effect(&ChatEffect::SetUpPersonalAssistant),
                ))
                .content(window),
        )
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// `composer`: a multi-line draft typed in the composer, the person's
    /// own until sent. `mention`: someone typing `@a` in the composer, the
    /// editor's `@` list open under it.
    #[cfg(feature = "samples")]
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
        if name == Some("msgmenu") {
            // The hovered message's bar is up; click its More as a person
            // does, so the menu opens where that click opens it.
            input.relayout();
            let more = input
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.label() == Some("More"))
                .and_then(|(_, node)| node.bounds())
                .ok_or("the message's bar shows no More button")?;
            input.click_at(
                f64::midpoint(more.x0, more.x1),
                f64::midpoint(more.y0, more.y1),
            );
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
    /// the capture's own person: its channels under the fixed ids the
    /// scenarios name, which only the moderators' actions make, minting their
    /// own, and its members named as its member list would name them.
    /// Their messages are staged in the chat's parts (`part_scenario`).
    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut ui::framework::Fixture<Chat>) {
        if fixture.scenario() == Some("starter") {
            return;
        }
        ui::tilo_resource::members::know(crate::demo::demo_members(crate::model::ME));
        fixture.start(crate::demo::demo());
    }

    /// The first-run workspace for `starter`, read by the capture's own
    /// person; every other scenario starts from its [`Self::fixture`].
    #[cfg(feature = "samples")]
    fn scenario(name: Option<&str>) -> Vec<Invoke<Chat>> {
        if name != Some("starter") {
            return Vec::new();
        }
        vec![Chat::BEGIN_CHAT.with(chat::BeginChat {})]
    }

    /// A new resource's creator, and the workspace's first channels, `#chat` and
    /// `#voice`, when it has none: under fixed ids and with the same values
    /// on every device, so concurrent first opens on several devices
    /// converge on exactly those two. A capture's resource is its scenario's
    /// instead.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Chat>> {
        if cx.host().is_some_and(|host| host.capture().is_some()) {
            return Vec::new();
        }
        vec![Chat::BEGIN_CHAT.with(chat::BeginChat {})]
    }
}

/// Navigation from a conversation to the workspace apps that own the person's settings and
/// proposals. Assistant can also set up the person's agent when Chat cannot read the roster.
fn conversation_setup(cx: &ViewCx<Chat>, chat: &SharedChat, window: Element) -> Element {
    use ui::tilo_reference::ResourceRef;
    use ui::widget::arrange::{Fit, Stack};
    use ui::widget::{Button, When};

    if !cx.mode().is_live()
        || cx
            .me()
            .and_then(|me| me.person_id())
            .is_none_or(str::is_empty)
    {
        return window;
    }
    let open_assistant = cx.dispatcher();
    let assistant = Button::new(
        "chat-open-assistant",
        &crate::i18n::t("chat-open-assistant"),
    )
    .opening_workspace_app(ui::apps::ASSISTANT)
    .on_view(move || open_assistant.open_workspace_app(ui::apps::ASSISTANT));
    let at_home = Rc::clone(chat);
    let assistant = When::new(assistant.into_widget(), move || {
        let chat = at_home.borrow();
        chat.alone.is_none()
            && chat.conversation_agent().is_none()
            && chat.agents_record().is_none_or(|agents| !agents.may_read())
    });

    let open_item = |id: &str, app: &'static str, item: ResourceRef| {
        let dispatch = cx.dispatcher();
        Button::new(id, &item.label)
            .opening_workspace_item(app, &item)
            .on_view(move || dispatch.open_reference(app, None, &item))
            .into_widget()
    };
    let settings = open_item(
        "chat-connect-model-provider",
        ui::apps::SETTINGS,
        crate::model_choice::settings_reference(),
    );
    let in_chat = Rc::clone(chat);
    let settings = When::new(settings, move || {
        let chat = in_chat.borrow();
        if chat.alone.is_some() || chat.conversation_agent().is_none() {
            return false;
        }
        chat.models.poll();
        !chat.models.has_choices()
    });
    let proposals = open_item(
        "chat-review-proposals",
        ui::apps::ASSISTANT,
        ResourceRef::whole(
            "assistant.section",
            "assistant.section:proposals",
            crate::i18n::t("chat-review-proposals"),
        ),
    );
    let in_direct = Rc::clone(chat);
    let proposals = When::new(proposals, move || {
        let chat = in_direct.borrow();
        let Some(keeper) = chat.me.id.person_id() else {
            return false;
        };
        let Some(agent) = chat.conversation_agent() else {
            return false;
        };
        let peer = ui::tilo_resource::MemberId::agent(agent.clone());
        if chat.alone.is_some()
            || Chat::direct_id([chat.me.id.clone(), peer]).as_str() != chat.active_channel
        {
            return false;
        }
        // A confined host can show this link without disclosing the roster: Assistant
        // opens the signed-in person's proposals, with its own authorization.
        chat.agents_record()
            .filter(|record| record.may_read())
            .is_none_or(|record| {
                record
                    .get()
                    .personal_assistant(keeper)
                    .is_some_and(|assistant| assistant.id.0 == agent)
            })
    });
    let controls = Stack::row()
        .wrap()
        .push(Box::new(assistant))
        .push(Box::new(settings))
        .push(Box::new(proposals));
    Box::new(
        Stack::stack()
            .gap(ui::tokens::Rhythm::None)
            .fill_width()
            .fill_height()
            .push(Box::new(controls))
            .push_fit(window, Fit::fill(1.0)),
    )
}

/// The messages a capture of `scenario` shows, staged in the chat's parts
/// once the window holds its chain ([`ChatApp::stage_parts`]): the first
/// part's messages as it starts, by several people under the fixed ids the
/// scenario names, which no one person's post makes; then what is done
/// with them, each by who did it. The first-run workspace has none.
#[cfg(feature = "samples")]
pub(crate) fn part_scenario(
    scenario: Option<&str>,
) -> (
    Option<ChatMessages>,
    Vec<(ui::tilo_resource::MemberId, Invoke<ChatMessages>)>,
) {
    let me = crate::model::ME;
    if scenario == Some("starter") {
        return (None, Vec::new());
    }
    let mut start = crate::demo::demo_part(me);
    let mut steps = Vec::new();
    match scenario {
        Some("long-thread") => {
            // Three days of the design crit before the conversation the demo
            // shows, so a capture of the list is a capture of a thread no window
            // holds.
            let mut part = ChatMessages::default();
            for (id, message) in long_thread_posts(me) {
                part.messages.push(id, message);
            }
            for (id, message) in start.messages.iter() {
                part.messages.push(id.clone(), message.clone());
            }
            start = part;
        }
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
                .map(|invoke| (ui::tilo_resource::MemberId::person(me), invoke)),
        ),
        Some(scenario @ ("agent-work" | "agent-stopped")) => {
            let (id, message) = agent_summons();
            start.messages.push(id, message);
            steps.extend(agent_work_part_steps());
            if scenario == "agent-stopped" {
                // Its person stops the work, and nothing has ended it yet: the
                // reply says who stopped it all the same.
                steps.push((
                    ui::tilo_resource::MemberId::person(me),
                    ChatMessages::STOP_AGENT.with(chat_messages::StopAgent {
                        message: Id::from_raw(DEMO_SUMMONS),
                    }),
                ));
            }
        }
        Some("agent-went-on") => {
            for (id, message) in went_on_messages() {
                start.messages.push(id, message);
            }
            steps.extend(went_on_part_steps());
        }
        _ => {}
    }
    (Some(start), steps)
}

/// `long-thread`: three days of messages in the design crit, from the workspace's
/// people, before the conversation the demo shows.
#[cfg(feature = "samples")]
fn long_thread_posts(me: &str) -> Vec<(Id, crate::chat::Message)> {
    let people = ["u_aria", "u_milo", "u_sora", "u_devon"];
    let count = 240_usize;
    // The newest of them ends where the demo conversation begins.
    let newest = crate::demo::NOW
        .saturating_sub(100 * 60)
        .saturating_mul(1_000);
    (0..count)
        .map(|index| {
            let ago = i64::try_from(count.saturating_sub(index))
                .unwrap_or_default()
                .saturating_mul(900_000);
            let author = if index % 5 == 4 {
                me
            } else {
                // `people` is not empty, so the cycle always names one.
                people
                    .get(index % people.len())
                    .copied()
                    .unwrap_or_default()
            };
            let text = if index % 7 == 3 {
                format!(
                    "Note {index}: the tile promotion needs the transport notes beside it, so \
                     the presenter keeps the featured tile while everyone else drops to the \
                     filmstrip beneath it."
                )
            } else {
                format!("Note {index}: the overlay reads fine at this width, and a few of these words wrap.")
            };
            (
                Id::from_raw(format!("m_long_{index:03}")),
                crate::chat::Message::new(
                    Id::from_raw("c_crit"),
                    ui::tilo_resource::MemberId::person(author),
                    newest.saturating_sub(ago),
                    text,
                ),
            )
        })
        .collect()
}

/// Provider output fixtures use the same durable action as a live host stream.
#[cfg(feature = "samples")]
pub(crate) fn provider_stream_scenario(scenario: &str) -> Vec<Invoke<ChatMessages>> {
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
            ChatMessages::STREAM_OUTPUT.with(chat_messages::StreamOutput {
                channel: Id::from_raw("c_crit"),
                reply: "shot_agent_stream".into(),
                name: ui::tilo_resource::NonBlank::parse(&crate::i18n::t("chat-model-author")),
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
#[cfg(feature = "samples")]
fn mention_post() -> (Id, crate::chat::Message) {
    let text = "@You could you check the hover states on the new sidebar before standup?";
    let post = crate::chat::Message {
        mentions: vec![crate::mention::Mention {
            user: ui::tilo_resource::MemberId::person(crate::model::ME),
            start: 0,
            len: "@You".len(),
            is_agent: false,
        }],
        ..crate::chat::Message::new(
            Id::from_raw("c_frontend"),
            ui::tilo_resource::MemberId::person("u_aria"),
            crate::demo::NOW.saturating_mul(1_000) - 30_000,
            text.into(),
        )
    };
    (Id::from_raw("m_mention"), post)
}

/// The bridge the `bridged` scenario's post is by.
#[cfg(feature = "samples")]
const DEMO_BRIDGE: &str = "5d0c7a3e-2b41-4f6a-9e18-c3a7b2d4e6f1";

/// `bridged`: a bridge's post of a line someone wrote on Discord, shown
/// under their name and picture while it stays the bridge's.
#[cfg(feature = "samples")]
fn bridged_post() -> (Id, crate::chat::Message) {
    let post = crate::chat::Message {
        appearance: Some(crate::chat::ShownAs {
            name: "Bo".into(),
            avatar: Some("tilo-demo://avatar-bo".into()),
            persona: Some(crate::chat::ShownPersona {
                service: "discord".into(),
                account: "80351110224678912".into(),
                linked: None,
            }),
        }),
        ..crate::chat::Message::new(
            Id::from_raw("c_crit"),
            ui::tilo_resource::MemberId::bot(DEMO_BRIDGE),
            crate::demo::NOW.saturating_mul(1_000) - 60_000,
            "Joining from Discord: the warmer palette reads much better on my phone.".into(),
        )
    };
    (Id::from_raw("m_bridged"), post)
}

/// The agent the `agent-work` scenario summons.
#[cfg(feature = "samples")]
pub(crate) fn demo_agent() -> ui::tilo_resource::MemberId {
    ui::tilo_resource::MemberId::agent("tide")
}
/// The message summoning it.
#[cfg(feature = "samples")]
pub(crate) const DEMO_SUMMONS: &str = "m_agent_request";

/// In the demo conversation: the reader summoning the agent, under the id
/// [`DEMO_SUMMONS`] the agent's resource of its work names.
#[cfg(feature = "samples")]
pub(crate) fn agent_summons() -> (Id, crate::chat::Message) {
    let summons = crate::chat::Message {
        mentions: vec![crate::mention::Mention {
            user: demo_agent(),
            start: 0,
            len: "@Tide".len(),
            is_agent: true,
        }],
        ..crate::chat::Message::new(
            Id::from_raw("c_crit"),
            ui::tilo_resource::MemberId::person(crate::model::ME),
            crate::demo::NOW.saturating_mul(1_000) - 120_000,
            "@Tide add the revenue curve to Calculator and keep the current bounds.".into(),
        )
    };
    (Id::from_raw(DEMO_SUMMONS), summons)
}

/// On the demo conversation holding [`agent_summons`]: the agent's resource
/// of the work it took on, by the agent.
#[cfg(feature = "samples")]
pub(crate) fn agent_work_part_steps() -> Vec<(ui::tilo_resource::MemberId, Invoke<ChatMessages>)> {
    vec![(demo_agent(), agent_work(&Id::from_raw(DEMO_SUMMONS)))]
}

/// The person's first message in the `agent-went-on` scenario, and their second.
#[cfg(feature = "samples")]
pub(crate) const WENT_ON_FIRST: &str = "m_went_on_first";
#[cfg(feature = "samples")]
pub(crate) const WENT_ON_SECOND: &str = "m_went_on_second";

/// The conversation of the `agent-went-on` scenario: the demo's person with their agent.
#[cfg(feature = "samples")]
pub(crate) fn went_on_conversation() -> Id {
    Chat::direct_id([
        ui::tilo_resource::MemberId::person(crate::model::ME),
        demo_agent(),
    ])
}

/// `agent-went-on`: the person wrote again while their agent worked. Under their first
/// message stands the reply its turn left when their second message took the turn along,
/// as the turn ends it: complete, with no word and no step. Their second message follows;
/// its answer is the agent's own ([`went_on_part_steps`]).
#[cfg(feature = "samples")]
pub(crate) fn went_on_messages() -> Vec<(Id, crate::chat::Message)> {
    let conversation = went_on_conversation();
    let me = ui::tilo_resource::MemberId::person(crate::model::ME);
    let first = Id::from_raw(WENT_ON_FIRST);
    let asked = crate::demo::NOW.saturating_sub(120).saturating_mul(1_000);
    let left = crate::chat::Message {
        parent: Some(first.clone()),
        work: Some(crate::chat::Work {
            state: Some(crate::chat::Transition {
                state: crate::model::AgentWorkState::ProviderFinished {
                    outcome: crate::agent_stream::AgentFinish::Complete,
                },
                since: asked + 10_000,
            }),
            ..crate::chat::Work::default()
        }),
        ..crate::chat::Message::new(
            conversation.clone(),
            demo_agent(),
            asked + 10_000,
            String::new(),
        )
    };
    vec![
        (
            first.clone(),
            crate::chat::Message::new(
                conversation.clone(),
                me.clone(),
                asked,
                "Draft a note to Maya about Friday's call.".into(),
            ),
        ),
        (crate::chat::reply_id(&first, &demo_agent()), left),
        (
            Id::from_raw(WENT_ON_SECOND),
            crate::chat::Message::new(
                conversation,
                me,
                asked + 60_000,
                "And say that I bring the numbers.".into(),
            ),
        ),
    ]
}

/// On the conversation holding [`went_on_messages`]: the one answer, under the person's
/// second message, by the agent.
#[cfg(feature = "samples")]
pub(crate) fn went_on_part_steps() -> Vec<(ui::tilo_resource::MemberId, Invoke<ChatMessages>)> {
    vec![(
        demo_agent(),
        work_noted(
            &Id::from_raw(WENT_ON_SECOND),
            crate::chat::WorkUpdate {
                state: "provider_complete".into(),
                app: None,
                did: None,
                about: None,
                trace: Vec::new(),
                detail: None,
                answered: None,
            },
            Some(
                "The note to Maya is drafted. It says that you bring the numbers on Friday.".into(),
            ),
        ),
    )]
}

/// The demo agent's resource of the work `summons` asked of it.
#[cfg(feature = "samples")]
pub(crate) fn agent_work(summons: &Id) -> Invoke<ChatMessages> {
    work_noted(
        summons,
        crate::chat::WorkUpdate {
            state: "doing".into(),
            app: None,
            did: Some("Add function".into()),
            about: Some("Revenue curve, preserving the current graph bounds".into()),
            trace: vec![crate::chat::StepUpdate {
                id: "00000000-0000-4000-8000-000000000001".into(),
                sequence: 0,
                resource: "calculator.workspace".into(),
                label: "Add function".into(),
                outcome: "asked".into(),
                why: None,
            }],
            detail: None,
            answered: None,
        },
        None,
    )
}

/// What an agent notes of its work on `summons`, with its reply's words where it has some:
/// Chat's own action, as an agent invokes it.
#[cfg(feature = "samples")]
pub(crate) fn work_noted(
    summons: &Id,
    work: crate::chat::WorkUpdate,
    say: Option<String>,
) -> Invoke<ChatMessages> {
    ChatMessages::RECORD_WORK.with(chat_messages::RecordWork {
        message: summons.clone(),
        work,
        say,
    })
}

/// Answer one of the chat's effects, invoked by a control, the palette, an
/// agent or a script, from and on the window `chat`.
fn answer(chat: &SharedChat, effect: ChatEffect, cx: &mut EffectCx<Chat>) -> EffectAnswer {
    match &effect {
        // A direct conversation is one of the chat's own: its id names the
        // two people it is between, so opening and sending read and write the
        // same resource and the same parts as a channel's.
        ChatEffect::OpenDirectConversation { public_key } => {
            let (_, id) = chat.borrow().direct_conversation(public_key)?;
            chat.borrow_mut().open_conversation(&id, &cx.live().get());
            return Ok(ui::framework::Effected::done());
        }
        ChatEffect::OpenAgent { agent } => {
            let state = cx.live().get();
            let mut chat = chat.borrow_mut();
            chat.open_agent(agent, &state);
            return Ok(ui::framework::Effected::done());
        }
        ChatEffect::SetUpPersonalAssistant => return chat.borrow().set_up_personal_assistant(cx),
        ChatEffect::SendDirectMessage { public_key, text } => {
            let (peer, id) = chat.borrow().direct_conversation(public_key)?;
            if text.trim().is_empty() {
                return Err(crate::i18n::t("chat-person-chat-empty"));
            }
            // Answered once the message is written: at once, or on a device back
            // from away once the conversation it holds reached the app.
            let reply = cx.reply();
            let accepted = serde_json::json!({"peer": peer, "accepted": true});
            chat.borrow()
                .post_to_conversation_then(&id, text, move |posted| {
                    reply.finish(posted.map(|()| tilo_actions::ActionOutput {
                        value: Some(accepted),
                        resources: Vec::new(),
                    }));
                });
            let mut chat = chat.borrow_mut();
            if chat.active_channel == id.as_str() {
                let held = chat.held_messages();
                chat.messages = cx.live().get().message_rows(&held, id.as_str());
                chat.scroll = 0.0;
                chat.request_relayout();
            }
            return Ok(ui::framework::Effected::Pending);
        }
        ChatEffect::Send {
            to,
            attached,
            text,
            parent,
            body,
            opens,
        } => {
            let state = cx.live().get();
            return chat.borrow().send_message(
                crate::direct::SendMessage {
                    to,
                    attached: attached.clone(),
                    text,
                    parent: parent.clone(),
                    body: body.clone(),
                    opens: opens.unwrap_or_default(),
                },
                &state,
            );
        }
        _ => {}
    }
    if let ChatEffect::ExportContent { channel, format } = &effect {
        return chat
            .borrow()
            .export_transcript(&cx.live().get(), channel, format.as_deref());
    }
    if let Some(url) = crate::external::browser_destination(&effect)? {
        return Ok(cx.thread(
            move |_| ui::widget::try_open_external_url(&url).map_err(|error| error.to_string()),
            |(), opened, _| opened.map(|()| ui::framework::Effected::done()),
        ));
    }
    chat.borrow_mut().answer_effect(effect)
}

impl ChatApp {
    /// Keep `channel`'s transcript as content ([`ChatEffect::ExportContent`]).
    fn export_transcript(&self, chat: &Chat, channel: &str, format: Option<&str>) -> EffectAnswer {
        let id = Id::from_raw(channel);
        let found = chat
            .channels
            .get(&id)
            .filter(|found| found.is_channel())
            .ok_or_else(|| format!("there is no channel `{channel}` in this chat"))?;
        let held = self.held_messages();
        // Oldest first, in the order the window shows the channel.
        let messages: Vec<_> = held
            .iter()
            .filter(|(_, message)| message.channel == id)
            .collect();
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
mod agent_conversation_tests;
#[cfg(test)]
mod tests;

// A long thread publishes thousands of actions (each message offers its
// own), so every share asks its part of them. Each share drives every
// scenario and takes its slice of the actions, so the shares are small
// enough to stay well inside one test's time limit on a loaded builder.
ui::reach_tests!(
    ChatShell,
    shares: [
        share_0, share_1, share_2, share_3, share_4, share_5, share_6, share_7, share_8, share_9,
        share_10, share_11, share_12, share_13, share_14, share_15, share_16, share_17, share_18,
        share_19, share_20, share_21, share_22, share_23,
    ],
    // A failed or reasoned agent answer publishes a row's actions per event,
    // as the long threads do; every share takes its part of each. A thread
    // of 7,663 actions split twelve ways still costs each share more than a
    // minute on a loaded builder, so the shares stay small.
    long: ["agent-failed", "agent-reasoning", "long-thread", "agent-long-thread"],
);
