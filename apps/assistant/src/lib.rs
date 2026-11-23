//! Assistant: one window onto a person's assistant.
//!
//! The assistant is not a private app record. It is the agent the person keeps in the
//! workspace (an item of the workspace's `tilo.workspace.agent` resource), its thread is the
//! Chat conversation between the two, its work is issues in Tasks and its progress work is
//! Shortcuts the host runs. The window shows those resources and changes them only through
//! the actions their keepers publish: the main chat and the sub-agents on the left, the
//! conversation in the middle, drawn by Chat itself, tasks and scheduled work beside it, and
//! a supervision of what it runs on.

pub use ui::apps::ASSISTANT as APP_ID;
pub const APP_TITLE: &str = "Assistant";

mod conversation;
mod effects;
pub mod locales;
mod panel;
#[cfg(feature = "samples")]
mod sample;
mod sidebar;
mod sources;
mod supervision;

use std::rc::Rc;

use ui::floating_surface::{Recipient, RecipientKind};
use ui::framework::Call;
use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_reference::ResourceRef;
use ui::tilo_resource::NonBlank;
use ui::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents, workspace_agents};
use ui::widget::ButtonRole;

pub use assistant_model::Window;
pub use effects::AssistantEffect;
use sources::{Assistant, Read, Reads};

/// The resource Chat publishes a workspace's conversations under, and its action that sends
/// a message to whom it names.
const CHAT_WORKSPACE: &str = "chat.workspace";
const CHAT_SEND: &str = "send";

/// The page of Settings where a person links their model providers and chooses the model each
/// kind of work runs on, as Settings publishes it: the kind of a page, and that page.
const SETTINGS_PAGE: &str = "settings.section";
const SETTINGS_MODELS: &str = "settings.section:agent_models";

/// The agent's transforms, answered by this device's engine under the host's own provider
/// (`tilo_workspace_service::agents::assistant::transforms::PROVIDER`), and the action that takes
/// a request: an answer in place, or the work it names.
const TRANSFORMS: &str = "tilo.agent.transforms";
const TRANSFORM_RESOURCE: &str = "agent.transform:default";
const TRANSFORM_RESPOND: &str = "respond";

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    MainChat,
    /// The sub-agent of this id.
    SubAgent(String),
    Supervision,
}

impl Place {
    const MAIN_CHAT: &'static str = "main-chat";
    const SUPERVISION: &'static str = "supervision";
    const SUB_AGENT: &'static str = "sub-agent:";

    /// The place's name among the sidebar's.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::MainChat => Self::MAIN_CHAT.to_owned(),
            Self::SubAgent(id) => format!("{}{id}", Self::SUB_AGENT),
            Self::Supervision => Self::SUPERVISION.to_owned(),
        }
    }

    /// The place `key` names; the main chat for any other.
    fn of(key: Option<&str>) -> Self {
        match key {
            Some(Self::SUPERVISION) => Self::Supervision,
            Some(key) => key
                .strip_prefix(Self::SUB_AGENT)
                .filter(|id| !id.is_empty())
                .map_or(Self::MainChat, |id| Self::SubAgent(id.to_owned())),
            None => Self::MainChat,
        }
    }
}

/// The window's view state, shared by its parts: never stored or synced.
#[derive(Clone)]
pub(crate) struct Nav {
    pub(crate) place: LocalState<Option<String>>,
    pub(crate) pane: LocalState<ui::widget::NavPane>,
    /// Whether the tasks panel shows.
    pub(crate) panel: LocalState<bool>,
    pub(crate) tab: LocalState<panel::Tab>,
    /// Whether the window asks for a sub-agent's job.
    pub(crate) asking: LocalState<bool>,
    /// The job as typed so far.
    pub(crate) job: LocalState<String>,
    /// What the agent answered the person's request with, where it answered it in place.
    pub(crate) answer: LocalState<String>,
    /// Moves when this window changed a resource it reads, so its parts read it again.
    pub(crate) changed: LocalState<u64>,
}

impl Nav {
    pub(crate) fn place(&self) -> Place {
        Place::of(self.place.get().as_deref())
    }

    /// Show `place`, as choosing it in the sidebar does.
    pub(crate) fn show(&self, place: &Place) {
        self.asking.set(false);
        self.place.set(Some(place.key()));
        self.pane.set(ui::widget::NavPane::Content);
    }

    /// Ask the person for the job a sub-agent is started with.
    pub(crate) fn ask_for_job(&self) {
        self.asking.set(true);
        self.pane.set(ui::widget::NavPane::Content);
    }
}

/// What the window's controls do: every one through an action its keeper publishes, as the
/// person using the window.
#[derive(Clone)]
pub(crate) struct Hands {
    dispatcher: Dispatcher<Window>,
    pub(crate) reads: Reads,
    nav: Nav,
    /// The person the window is for: the name they go by here, and who they are.
    pub(crate) person: Option<(String, String)>,
    /// The embed the place in view asks the shell for.
    pub(crate) wanted: conversation::Wanted,
}

/// The name the window's own person goes by at its foot: theirs once the workspace or this
/// device names them, else "You", as Chat and History name them, never their key or the
/// placeholder others see for someone unnamed.
fn own_name(cx: &ViewCx<Window>, me: &ui::tilo_resource::MemberId) -> String {
    let name = cx.people().name(me);
    if name.trim().is_empty() || name == me.id() || name == ui::presence::unnamed_person_label() {
        return tr("assistant-person-you");
    }
    name
}

impl Hands {
    fn of(cx: &ViewCx<Window>, nav: &Nav) -> Self {
        Self {
            dispatcher: cx.dispatcher(),
            reads: Reads::of(cx),
            nav: nav.clone(),
            person: cx.me().map(|me| (own_name(cx, me), me.id().to_owned())),
            wanted: Rc::default(),
        }
    }

    /// Make the workspace's assistant: an agent of the workspace's own document of agents,
    /// through that resource's own `create-agent`, and set to run on the workspace's host: an
    /// assistant answers while every member's device is off, so no device runs it, wherever it
    /// was made. Its model is the workspace's choice.
    ///
    /// # Errors
    /// The workspace's agents are not held here, or the workspace's grants refuse.
    pub(crate) fn create_assistant(&self) -> Result<(), String> {
        let unreadable = || tr("assistant-agents-not-here");
        let agents = self.reads.agents().ok_or_else(unreadable)?;
        let name = NonBlank::parse(&tr("assistant-name")).ok_or_else(unreadable)?;
        let id = agents.get().free_id(name.as_str());
        agents
            .invoke(
                &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    name,
                    id: Some(id),
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())?;
        self.nav
            .changed
            .update(|changed| *changed = changed.wrapping_add(1));
        Ok(())
    }

    /// Start a sub-agent for `job`, as the window's own effect does: the palette, an agent
    /// and the window's button run the same thing.
    pub(crate) fn start_sub_agent(&self, job: String) {
        self.dispatcher
            .effect(&AssistantEffect::StartSubAgent { job });
    }

    /// Handing the main agent `job` for a sub-agent: one message in the main chat, as the
    /// person, through Chat's own `send`. The main agent picks the role and the model.
    ///
    /// # Errors
    /// There is no job, or no assistant to hand it to.
    fn job_for_main_agent(&self, job: &str) -> Result<Call, String> {
        let job = job.trim();
        if job.is_empty() {
            return Err(tr("assistant-job-empty"));
        }
        let Read::Here(Some(assistant)) = self.reads.assistant() else {
            return Err(tr("assistant-none"));
        };
        let to = [Recipient::found(
            RecipientKind::Agent,
            assistant.id,
            assistant.name,
        )];
        let chat = ResourceRef::whole(ui::apps::CHAT, CHAT_WORKSPACE, "Chat");
        Ok(
            Call::on(ui::apps::CHAT, None, &chat, CHAT_SEND).with(serde_json::json!({
                "to": to,
                "attached": [],
                "text": trf(
                    "assistant-start-sub-agent-message",
                    &[("words", job.to_owned().into())],
                ),
                "opens": false,
            })),
        )
    }

    /// Open Chat on the conversation with `assistant`, as a window of the person's own.
    pub(crate) fn open_in_chat(&self, assistant: &Assistant) {
        self.dispatcher.open_reference(
            ui::apps::CHAT,
            None,
            &ResourceRef::whole(
                workspace_agent::KIND,
                conversation::agent_resource(&assistant.id),
                assistant.name.clone(),
            ),
        );
    }

    /// Open Settings at Agent models, where the person's model providers and the model each
    /// kind of work runs on are chosen.
    fn open_settings(&self) {
        self.dispatcher.open_reference(
            ui::apps::SETTINGS,
            None,
            &ResourceRef::whole(SETTINGS_PAGE, SETTINGS_MODELS, "Settings"),
        );
    }
}

pub(crate) fn tr(key: &str) -> String {
    crate::locales::CATALOG.t(key)
}

pub(crate) fn trf(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::locales::CATALOG.t_args(key, args)
}

/// A page of the window: its content in a readable column that scrolls.
pub(crate) fn page(content: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Group);
    for part in content {
        column = column.push(part);
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(
                Container::new()
                    .readable()
                    .centered()
                    .with_inset(ui::tokens::Rhythm::Group)
                    .with_child(Box::new(column)),
            )),
    )
}

pub struct App;

impl ResourceApp for App {
    type Resource = Window;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const TITLE_KEY: &'static str = "assistant-app-title";

    fn resources(resources: &mut ui::framework::Resources<'_>) {
        // The workspace's agents are owned by Assistant: every app of the
        // workspace opens them, and their changes are decided against
        // Assistant's surface.
        resources.shared_workspace::<ui::tilo_workspace_agent::WorkspaceAgents>();
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 820)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut Fixture<Window>) {
        sample::fixture(fixture);
    }

    fn view(cx: &mut ViewCx<Window>) -> Element {
        let nav = Nav {
            place: cx.local(Some(Place::MainChat.key())),
            pane: cx.local(ui::widget::NavPane::Content),
            panel: cx.local(true),
            tab: cx.local(panel::Tab::Active),
            asking: cx.local(false),
            job: cx.local(String::new()),
            answer: cx.local(String::new()),
            changed: cx.local(0),
        };
        #[cfg(feature = "samples")]
        {
            let stage = nav.clone();
            cx.on_stage(move |scenario| sample::stage(scenario, &stage));
        }
        let hands = Hands::of(cx, &nav);
        {
            let (nav, hands) = (nav.clone(), hands.clone());
            cx.effects(move |effect: AssistantEffect, cx| answer(effect, &nav, &hands, cx));
        }
        let changed = nav.changed.clone();
        cx.switch(&changed, move |cx, _| {
            let (nav, hands) = (nav.clone(), hands.clone());
            let place = nav.place.clone();
            cx.switch(&place, move |cx, _| {
                let (nav, hands) = (nav.clone(), hands.clone());
                let asking = nav.asking.clone();
                cx.switch(&asking, move |cx, _| window(cx, &nav, &hands))
            })
        })
    }
}

/// Answer `effect` on the window `nav` shows.
fn answer(
    effect: AssistantEffect,
    nav: &Nav,
    hands: &Hands,
    cx: &mut EffectCx<Window>,
) -> ui::framework::EffectAnswer {
    match effect {
        AssistantEffect::ViewMainChat => nav.show(&Place::MainChat),
        AssistantEffect::ViewSubAgent { agent } => {
            let listed = matches!(
                hands.reads.sub_agents(),
                Read::Here(listed) if listed.iter().any(|sub| sub.id == agent)
            );
            if !listed {
                return Err(tr("assistant-no-such-sub-agent"));
            }
            nav.show(&Place::SubAgent(agent));
        }
        AssistantEffect::ViewSupervision => nav.show(&Place::Supervision),
        AssistantEffect::StartSubAgent { job } => {
            // The agent decides what the request is before a sub-agent is made for it: a
            // question it can answer it answers here, in the composer, and work it names starts
            // a sub-agent, as before. When it cannot be asked at all, the request goes as it
            // stands and the run decides.
            let ask = respond_to(&job)?;
            let nav = nav.clone();
            let hands = hands.clone();
            return Ok(cx.call(&ask, move |(), decided, cx| {
                let decision = decided
                    .ok()
                    .and_then(|output| Decision::of(output.value.as_ref()));
                match decision {
                    Some(Decision::Answer(said)) => {
                        hands.nav.answer.set(said);
                        Ok(Effected::done())
                    }
                    Some(Decision::Work) | None => hand_over(&hands, &nav, &job, cx),
                }
            }));
        }
        AssistantEffect::OpenModelSettings => hands.open_settings(),
    }
    Ok(Effected::done())
}

/// What the agent decided a person's request is: the answer to it, given in place, or the work
/// it names.
enum Decision {
    Answer(String),
    Work,
}

impl Decision {
    /// The decision `result` holds, held to the shape the action promises: one of `answer` or
    /// `work`, in words.
    fn of(result: Option<&serde_json::Value>) -> Option<Self> {
        let object = result?.as_object()?;
        if object.len() != 1 {
            return None;
        }
        let words = |key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|said| !said.is_empty())
                .map(str::to_owned)
        };
        words("answer")
            .map(Self::Answer)
            .or_else(|| words("work").map(|_| Self::Work))
    }
}

/// The call that asks the agent what a person's request is, through the host's own transforms:
/// it answers a question in place, or names the work it asks for.
///
/// # Errors
/// There is no request: an empty field asks nothing.
fn respond_to(request: &str) -> Result<Call, String> {
    let request = request.trim();
    if request.is_empty() {
        return Err(tr("assistant-job-empty"));
    }
    Ok(
        Call::host(TRANSFORMS, TRANSFORM_RESOURCE, TRANSFORM_RESPOND)
            .with(serde_json::json!({ "request": request })),
    )
}

/// Hand `job` to the assistant's main agent, which starts the sub-agent: the main chat is where
/// the message stands once Chat took it.
fn hand_over(
    hands: &Hands,
    nav: &Nav,
    job: &str,
    cx: &mut EffectCx<Window>,
) -> ui::framework::EffectAnswer {
    let send = hands.job_for_main_agent(job)?;
    let nav = nav.clone();
    Ok(cx.call(&send, move |(), sent, _cx| {
        sent?;
        nav.show(&Place::MainChat);
        Ok(Effected::done())
    }))
}

/// The window as it shows `nav`'s place: the places on the left, the place in the middle
/// with its commands in the header, and the tasks panel beside it.
fn window(cx: &ViewCx<Window>, nav: &Nav, hands: &Hands) -> Element {
    let assistant = hands.reads.assistant();
    let has_assistant = matches!(&assistant, Read::Here(Some(_)));
    let place = nav.place();
    // Supervision is of the assistant itself, so it stands beside the window's name: the
    // places under it are the conversations.
    let supervision: Element = {
        let nav = nav.clone();
        Box::new(
            Button::icon_only(
                "assistant-supervision",
                Icon::Gauge,
                &tr("assistant-supervision"),
            )
            .toggled(place == Place::Supervision)
            .on_view(move || {
                if nav.place() == Place::Supervision {
                    nav.show(&Place::MainChat);
                } else {
                    nav.show(&Place::Supervision);
                }
            }),
        )
    };
    let shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .with_id("assistant.shell")
        .mode(NavMode::Push)
        .with_pane_state(&nav.pane)
        .with_inspector_state(&nav.panel)
        .sidebar(sidebar::view(nav, hands))
        .sidebar_header(tr("assistant-title"), vec![supervision])
        .inspector(panel::view(cx, nav, hands))
        .inspector_header(tr("assistant-tasks"), Vec::new());
    let shell = if nav.asking.get() && has_assistant {
        shell
            .content(job_prompt(cx, nav, hands, &assistant))
            .content_header(tr("assistant-start-sub-agent"), Vec::new())
    } else {
        let start = |nav: &Nav| -> Vec<Element> {
            if !has_assistant {
                return Vec::new();
            }
            let asked = nav.clone();
            vec![Box::new(
                Button::icon_only(
                    "assistant-start-sub-agent",
                    Icon::Pencil,
                    &tr("assistant-start-sub-agent"),
                )
                .on_view(move || asked.ask_for_job()),
            )]
        };
        match &place {
            Place::MainChat => shell
                .content(main_chat(cx, hands, &assistant))
                .content_header(tr("assistant-main-chat"), start(nav)),
            Place::SubAgent(id) => {
                let (job, content) = sub_agent(hands, id);
                shell.content(content).content_header(job, start(nav))
            }
            Place::Supervision => shell
                .content(supervision::view(cx, hands, &assistant))
                .content_header(tr("assistant-supervision"), Vec::new()),
        }
    };
    Box::new(conversation::Embedding::around(
        Box::new(shell),
        Rc::clone(&hands.wanted),
    ))
}

/// The main chat: Chat's own view of the conversation with the assistant, or what stands in
/// its place while the person has none.
fn main_chat(cx: &ViewCx<Window>, hands: &Hands, assistant: &Read<Option<Assistant>>) -> Element {
    match assistant {
        Read::Here(Some(assistant)) => conversation::view(cx, hands, assistant),
        Read::Here(None) => no_assistant(cx, hands),
        Read::NotHere => agents_not_here(),
    }
}

/// What stands where the assistant would while the workspace's agents cannot be read.
pub(crate) fn agents_not_here() -> Element {
    Box::new(
        EmptyState::new(tr("assistant-agents-not-here"))
            .with_icon(Icon::Bot)
            .with_id("assistant-agents-not-here"),
    )
}

/// What stands where the assistant would while the person keeps none: the one step that
/// makes it, and why that step did not go ahead when it did not.
pub(crate) fn no_assistant(cx: &ViewCx<Window>, hands: &Hands) -> Element {
    let refused = cx.local(String::new());
    let hands = hands.clone();
    let said = refused.clone();
    cx.switch(&refused, move |_cx, why| {
        let state = if why.is_empty() {
            EmptyState::new(tr("assistant-none")).with_icon(Icon::Bot)
        } else {
            EmptyState::error(tr("assistant-create-refused"), why.clone())
        };
        let (hands, said) = (hands.clone(), said.clone());
        Box::new(
            state
                .with_id("assistant-none")
                .with_action(tr("assistant-create"))
                .with_action_id("assistant-create")
                .on_effect(move || {
                    if let Err(why) = hands.create_assistant() {
                        said.set(why);
                    }
                }),
        )
    })
}

/// A sub-agent's place: its job as the header reads it, and its conversation.
fn sub_agent(hands: &Hands, id: &str) -> (String, Element) {
    let listed = match hands.reads.sub_agents() {
        Read::Here(listed) => listed.into_iter().find(|sub| sub.id == id),
        Read::NotHere => None,
    };
    match listed {
        // Under its job, what it is and the one rule of a sub-agent: the main agent reads it,
        // it never reads the main chat.
        Some(sub) => (
            sub.job,
            page(vec![
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_rhythm(ui::tokens::Rhythm::Tight)
                        .push(Box::new(
                            Text::new(&sub.role)
                                .with_role(TextRole::Caption)
                                .dimmed()
                                .with_id("assistant-sub-agent-role"),
                        ))
                        .push(Box::new(
                            Text::new(&tr("assistant-sub-agent-cannot-read-main"))
                                .with_role(TextRole::Caption)
                                .dimmed(),
                        )),
                ),
                Box::new(
                    EmptyState::new(tr("assistant-sub-agent-conversation-not-here"))
                        .with_icon(Icon::MessageCircle)
                        .with_id("assistant-sub-agent-conversation"),
                ),
            ]),
        ),
        None => (
            tr("assistant-sub-agents"),
            Box::new(
                EmptyState::new(tr("assistant-no-such-sub-agent"))
                    .with_id("assistant-sub-agent-gone"),
            ),
        ),
    }
}

/// Asking for a sub-agent's job: the job, and nothing else. The main agent picks the role
/// and the model.
fn job_prompt(
    cx: &ViewCx<Window>,
    nav: &Nav,
    hands: &Hands,
    assistant: &Read<Option<Assistant>>,
) -> Element {
    let name = match assistant {
        Read::Here(Some(assistant)) => assistant.name.clone(),
        Read::Here(None) | Read::NotHere => tr("assistant-name"),
    };
    let refused = cx.local(String::new());
    let job = TextInput::from_state(&nav.job)
        .with_id("assistant-job")
        .with_accessible_name(tr("assistant-job-placeholder"))
        .with_placeholder("assistant-job-placeholder")
        .multiline(true);
    let start = {
        let (nav, hands, said) = (nav.clone(), hands.clone(), refused.clone());
        Button::new("assistant-job-start", "assistant-job-start")
            .role(ButtonRole::Primary)
            .on_effect(move || {
                let job = nav.job.get();
                if job.trim().is_empty() {
                    said.set(tr("assistant-job-empty"));
                    return;
                }
                // The window stays here while the agent decides: it answers in place, or
                // the sub-agent starts and the main chat shows where the work stands.
                nav.answer.set(String::new());
                hands.start_sub_agent(job);
                nav.job.set(String::new());
            })
    };
    let cancel = {
        let nav = nav.clone();
        Button::new("assistant-job-cancel", "assistant-job-cancel")
            .on_view(move || nav.asking.set(false))
    };
    page(vec![
        Box::new(Text::new(&trf(
            "assistant-job-prompt",
            &[("name", name.into())],
        ))),
        Box::new(job),
        Box::new(
            Text::from_state(&nav.answer, |said: &String| said.clone())
                .with_id("assistant-answer")
                .with_role(TextRole::Body)
                .with_live(ui::a11y::Politeness::Polite),
        ),
        Box::new(
            Row::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(start))
                .push(Box::new(cancel)),
        ),
        Box::new(
            Text::from_state(&refused, |why: &String| why.clone())
                .with_id("assistant-job-refused")
                .with_role(TextRole::Caption)
                .dimmed()
                .with_live(ui::a11y::Politeness::Polite),
        ),
    ])
}

ui::dev_reload_entry!(App);

ui::reach_tests!(App);
