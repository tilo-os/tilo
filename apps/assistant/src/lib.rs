//! Manage a person's workspace assistant and its sub-agents. Conversations open in Chat,
//! where the keeper and the agent share an ordinary direct conversation.

pub use ui::apps::ASSISTANT as APP_ID;
pub const APP_TITLE: &str = "Assistant";

mod effects;
pub mod locales;
mod panel;
mod proposals;
#[cfg(feature = "samples")]
mod sample;
mod sidebar;
mod sources;
mod supervision;

use ui::floating_surface::{Recipient, RecipientKind};
use ui::framework::Call;
use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_reference::ResourceRef;
use ui::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents};
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

/// A named place that a workspace reference opens in the Assistant window.
#[derive(ui::tilo_resource::Row)]
#[row(kind = "assistant.section", label = "Assistant section")]
struct AssistantSection;

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    Overview,
    Proposals,
    /// The sub-agent of this id.
    SubAgent(String),
}

impl Place {
    const OVERVIEW: &'static str = "overview";
    const PROPOSALS: &'static str = "assistant.section:proposals";
    const SUB_AGENT: &'static str = "sub-agent:";

    /// The place's name among the sidebar's.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Overview => Self::OVERVIEW.to_owned(),
            Self::Proposals => Self::PROPOSALS.to_owned(),
            Self::SubAgent(id) => format!("{}{id}", Self::SUB_AGENT),
        }
    }

    fn of(key: Option<&str>) -> Self {
        if key == Some(Self::PROPOSALS) {
            return Self::Proposals;
        }
        key.and_then(|key| key.strip_prefix(Self::SUB_AGENT))
            .filter(|id| !id.is_empty())
            .map_or(Self::Overview, |id| Self::SubAgent(id.to_owned()))
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
        }
    }

    /// Ensure the caller has a personal assistant. The agents resource binds the keeper
    /// to the requesting person and applies their grants.
    pub(crate) fn create_assistant(&self, refused: &LocalState<String>) {
        let Some(agents) = self.reads.agents() else {
            refused.set(tr("assistant-agents-not-here"));
            return;
        };
        let changed = self.nav.changed.clone();
        let refused = refused.clone();
        agents.invoke_then(
            &WorkspaceAgents::ENSURE_PERSONAL_ASSISTANT.with(()),
            move |setup| match setup {
                Ok(()) => changed.update(|revision| *revision = revision.wrapping_add(1)),
                Err(why) => refused.set(why.to_string()),
            },
        );
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
    /// The job is empty.
    fn job_for_main_agent(assistant: &Assistant, job: &str) -> Result<Call, String> {
        let job = job.trim();
        if job.is_empty() {
            return Err(tr("assistant-job-empty"));
        }
        let to = [Recipient::found(
            RecipientKind::Agent,
            assistant.id.clone(),
            assistant.name.clone(),
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

    fn assistant(&self) -> Result<Assistant, String> {
        match self.reads.assistant() {
            Read::Here(Some(assistant)) => Ok(assistant),
            Read::Here(None) => Err(tr("assistant-none")),
            Read::NotHere => Err(tr("assistant-agents-not-here")),
        }
    }

    fn chat_button(&self, id: &str, name: &str) -> Element {
        let conversation = agent_reference(id, name);
        let dispatcher = self.dispatcher.clone();
        Box::new(
            Button::new("assistant-open-in-chat", "assistant-open-in-chat")
                .opening_workspace_item(ui::apps::CHAT, &conversation)
                .with_icon(Icon::MessageCircle)
                .role(ButtonRole::Primary)
                .on_view(move || dispatcher.open_reference(ui::apps::CHAT, None, &conversation)),
        )
    }
}

fn model_settings() -> ResourceRef {
    ResourceRef::whole(SETTINGS_PAGE, SETTINGS_MODELS, tr("assistant-model-change"))
}

/// Chat accepts the workspace agent's item as the address of the caller's DM, even
/// before it contains a first message.
fn agent_reference(id: &str, name: &str) -> ResourceRef {
    let at = ui::tilo_resource::items::item_resource_id::<WorkspaceAgents>("agents", &Id::from(id));
    ResourceRef::whole(workspace_agent::KIND, at, name)
}

fn open_main_chat(assistant: &Assistant, cx: &mut EffectCx<Window>) -> Effected {
    open_workspace_item(
        ui::apps::CHAT,
        &agent_reference(&assistant.id, &assistant.name),
        cx,
    )
}

fn open_workspace_item(app: &str, item: &ResourceRef, cx: &mut EffectCx<Window>) -> Effected {
    let open = Call::on(
        app,
        None,
        item,
        ui::tilo_actions::remote::APPLICATION_OPEN_ACTION,
    );
    cx.call(&open, |(), opened, _| {
        opened?;
        Ok(Effected::done())
    })
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
    const WORKSPACE_RESOURCE: bool = true;

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
            place: cx.local(Some(Place::Overview.key())),
            pane: cx.local(ui::widget::NavPane::Content),
            panel: cx.local(true),
            tab: cx.local(panel::Tab::Active),
            asking: cx.local(false),
            job: cx.local(String::new()),
            changed: cx.local(0),
        };
        #[cfg(feature = "samples")]
        {
            let stage = nav.clone();
            cx.on_stage(move |scenario| sample::stage(scenario, &stage));
        }
        let hands = Hands::of(cx, &nav);
        let opened = nav.clone();
        cx.on_open(move |anchor| {
            if anchor == Place::PROPOSALS {
                opened.show(&Place::Proposals);
                Ok(())
            } else {
                Err(ui::tilo_resource::Refusal::new(tr(
                    "assistant-section-missing",
                )))
            }
        });
        {
            let (nav, hands) = (nav.clone(), hands.clone());
            cx.effects(move |effect: AssistantEffect, cx| answer(effect, &nav, &hands, cx));
        }
        let changed = nav.changed.clone();
        let content = cx.switch(&changed, move |cx, _| {
            let (nav, hands) = (nav.clone(), hands.clone());
            let place = nav.place.clone();
            cx.switch(&place, move |cx, _| {
                let (nav, hands) = (nav.clone(), hands.clone());
                let asking = nav.asking.clone();
                cx.switch(&asking, move |cx, _| window(cx, &nav, &hands))
            })
        });
        let proposals = ui::framework::openable_row::<AssistantSection>(
            Place::PROPOSALS,
            tr("assistant-review-proposals"),
        );
        let listed = proposals.clone();
        cx.publishing(
            ui::framework::Publishes::new()
                .live_rows(move || vec![listed.clone()])
                .live_row(move |id| (id == proposals.id()).then(|| proposals.clone())),
            content,
        )
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
        AssistantEffect::ViewMainChat => return Ok(open_main_chat(&hands.assistant()?, cx)),
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
        AssistantEffect::ViewSupervision => nav.show(&Place::Overview),
        AssistantEffect::StartSubAgent { job } => {
            let assistant = hands.assistant()?;
            let send = Hands::job_for_main_agent(&assistant, &job)?;
            let nav = nav.clone();
            return Ok(cx.call(&send, move |(), sent, cx| {
                sent?;
                if nav.job.get() == job {
                    nav.job.set(String::new());
                }
                nav.show(&Place::Overview);
                Ok(open_main_chat(&assistant, cx))
            }));
        }
        AssistantEffect::OpenModelSettings => {
            return Ok(open_workspace_item(
                ui::apps::SETTINGS,
                &model_settings(),
                cx,
            ));
        }
    }
    Ok(Effected::done())
}

/// The window as it shows `nav`'s place: the places on the left, the place in the middle
/// with its commands in the header, and the tasks panel beside it.
fn window(cx: &ViewCx<Window>, nav: &Nav, hands: &Hands) -> Element {
    let assistant = hands.reads.assistant();
    let has_assistant = matches!(&assistant, Read::Here(Some(_)));
    let place = nav.place();
    let shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .with_id("assistant.shell")
        .mode(NavMode::Push)
        .with_pane_state(&nav.pane)
        .with_inspector_state(&nav.panel)
        .sidebar(sidebar::view(nav, hands))
        .sidebar_header(tr("assistant-title"), Vec::new())
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
            Place::Overview => shell
                .content(supervision::view(cx, hands, &assistant))
                .content_header(tr("assistant-overview"), start(nav)),
            Place::Proposals => shell
                .content(proposals::view(cx))
                .content_header(tr("assistant-review-proposals"), Vec::new()),
            Place::SubAgent(id) => {
                let (job, content) = sub_agent(hands, id);
                shell.content(content).content_header(job, start(nav))
            }
        }
    };
    Box::new(shell)
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
                    hands.create_assistant(&said);
                }),
        )
    })
}

/// A sub-agent's job and role, with a link to its direct conversation.
fn sub_agent(hands: &Hands, id: &str) -> (String, Element) {
    let listed = match hands.reads.sub_agents() {
        Read::Here(listed) => listed.into_iter().find(|sub| sub.id == id),
        Read::NotHere => None,
    };
    match listed {
        // Under its job, what it is and the one rule of a sub-agent: the main agent reads it,
        // it never reads the main chat.
        Some(sub) => (
            sub.job.clone(),
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
                hands.chat_button(&sub.id, &sub.job),
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
                if nav.asking.get() {
                    nav.show(&Place::Overview);
                    hands.start_sub_agent(job);
                }
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
