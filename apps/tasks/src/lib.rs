//! Tasks: the workspace's issues, on a list and a board.
//!
//! The tracker is the app's record of the workspace ([`model`]): nested
//! issues with a markdown description, a status, an assignee, labels, links
//! and a thread. The list nests sub-issues under their parent; the board has
//! a lane per status, and dragging a card is one `place` action; the issue
//! page edits the description in the shared block editor and holds the
//! thread. An issue assigned to an agent is that agent's task: its runs are
//! the issue's actions, approved, steered and answered from the issue page.
//! An issue's note is written in Notes, through Notes' own action (`note`).
//!
//! Storage, sync, history, undo, search and permissions are the framework's.
//!
//! The sidebar is the tracker read the way a person works through it: every
//! issue, the ones assigned to them, the ones nobody has taken, the ones an
//! agent is working on, one row per label in use, and the projects and areas
//! the work goes towards. A project's page reads its tasks, the documents
//! they carry and the people involved in it; those tasks stay in the list and
//! in their person's own list as they were. The window bar holds
//! the two places (Issues, Board) as its views and names the open issue as
//! its location, so the window draws no title row and no Back of its own.

/// The application id.
pub const APP_ID: &str = crate::model::APP_ID;
/// The window and launcher title.
pub const APP_TITLE: &str = "Tasks";

mod board;
mod chat;
mod filter;
mod issue;
mod list;
pub mod locales;
pub mod model;
mod note;
mod project;
mod runs;
#[cfg(feature = "samples")]
mod sample;
mod task_chip;
mod thread;
mod window_bar;

pub(crate) use filter::Filter;

use ui::framework::{EffectCx, Effected};
use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::widget::{NavItem, NavSection};

pub use chat::{SCHEME as CHAT_SCHEME, uri as chat_uri};
pub use model::Tracker;
use model::area_resource;
pub use note::{NOTES, TasksEffect};

/// What the window shows when no issue is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// Every issue, sub-issues under their parent.
    #[default]
    List,
    /// A lane per status.
    Board,
    /// The tasks the tracker holds a run for, all of them.
    Tasks,
    /// The tasks that have said something, as their conversations.
    Thread,
}

impl Place {
    /// The places the window shows, in the bar's order.
    pub(crate) const ALL: [Self; 4] = [Self::List, Self::Board, Self::Tasks, Self::Thread];

    /// The place as the window bar and the bar's view switcher name it.
    pub(crate) fn title(self) -> String {
        match self {
            Self::List => tr("tasks-place-list"),
            Self::Board => tr("tasks-place-board"),
            Self::Thread => tr("tasks-place-thread"),
            Self::Tasks => tr("tasks-place-tasks"),
        }
    }
}

/// The window's view state, shared by its parts: never stored or synced.
#[derive(Clone)]
pub(crate) struct Nav {
    pub(crate) place: LocalState<Place>,
    pub(crate) open: LocalState<Option<Id>>,
    /// The sidebar row chosen: a [`Filter`]'s key, or none for everything.
    pub(crate) filter: LocalState<Option<String>>,
    /// The project or area whose page is open.
    pub(crate) project: LocalState<Option<Id>>,
    /// The "New project" dialog is asking.
    pub(crate) asking: LocalState<bool>,
    /// The new project's name, while the dialog asks.
    pub(crate) named: LocalState<String>,
    /// The issue this window shows as a chat: the one its launch named
    /// ([`chat_uri`]). A chat window shows that conversation alone.
    pub(crate) chat: LocalState<Option<Id>>,
}

impl Nav {
    pub(crate) fn open(&self, issue: &Id) {
        self.open.set(Some(issue.clone()));
    }

    /// The project or area whose page is open.
    pub(crate) fn area(&self) -> Option<Id> {
        self.project.with(Option::clone)
    }

    /// The filter the sidebar has chosen.
    pub(crate) fn filter(&self) -> Filter {
        self.filter
            .with(|key| key.as_deref().and_then(Filter::of))
            .unwrap_or_default()
    }
}

pub(crate) fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

/// A message with named text arguments.
pub(crate) fn tr_with(key: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, ui::i18n::ArgValue::Text((*value).to_string())))
        .collect();
    I18nManager::format(key, &args)
}

pub struct App;

impl ResourceApp for App {
    type Resource = Tracker;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    /// A window is launched on nothing, or on the chat of one issue
    /// ([`chat_uri`]).
    const ARGUMENTS: usize = 1;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1240, 780)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    /// A capture cannot make six issues and label them: the scenario stages
    /// the tracker it reached as the record's start, and its views after.
    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut ui::framework::Fixture<Tracker>) {
        if fixture.scenario().is_some() {
            fixture.start(sample::tracker(fixture.person()));
        }
    }

    fn view(cx: &mut ViewCx<Tracker>) -> Element {
        let launched = cx
            .launch_arguments()
            .first()
            .and_then(|argument| chat::target(argument));
        let (asking, named) = (cx.local(false), cx.local(String::new()));
        let nav = Nav {
            place: cx.local(Place::List),
            open: cx.local(None),
            filter: cx.local(None),
            project: cx.local(None),
            asking,
            named,
            chat: cx.local(launched),
        };
        let addresses = router(cx, &nav);
        let chat = nav.chat.clone();
        cx.switch(&chat, move |cx, chat| match chat {
            Some(issue) => chat::window(cx, issue.clone(), &addresses),
            None => tracker_window(cx, &nav, &addresses),
        })
    }
}

/// The window over the whole tracker: the sidebar, the list, the board and
/// the issue pages.
fn tracker_window(cx: &ViewCx<Tracker>, nav: &Nav, addresses: &LocalState<Tracker>) -> Element {
    let sidebar = sidebar(cx, nav, addresses);
    let content_nav = nav.clone();
    let content = cx.switch(&nav.open, move |cx, open| {
        if let Some(issue) = open {
            return issue::page(cx, issue.clone(), &content_nav);
        }
        let nav = content_nav.clone();
        cx.switch(&content_nav.project, move |cx, project| {
            if let Some(project) = project {
                return project::page(cx, project.clone());
            }
            let nav = nav.clone();
            let content_nav = nav.clone();
            cx.switch(&content_nav.place, move |cx, place| match place {
                Place::List => list::view(cx, &nav),
                Place::Board => board::view(cx, &nav),
                Place::Thread => thread::view(cx, &nav),
                Place::Tasks => runs::view(cx, &nav),
            })
        })
    });
    let dialog = project::NewProject::new(cx, nav.asking.clone(), &nav.named);

    let (back, closer) = (nav.clone(), nav.clone());
    let shell = ui::widget::NavShell::new()
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("tasks.shell")
        .mode(ui::widget::NavMode::Push)
        .with_window_toolbar()
        .with_pane_state(&cx.local(ui::widget::NavPane::Content))
        .sidebar(sidebar)
        // The button that makes a project is the app's own published
        // effect, so a screen reader, a script and an agent open the form
        // as the press does. The window shows it when the effect lands.
        .sidebar_header(
            tr("tasks-title"),
            vec![Box::new(
                cx.effect_button("tasks-new-project", &TasksEffect::NewProject)
                    .with_icon(ui::icons::Icon::Plus)
                    .role(ui::widget::ButtonRole::Plain),
            )],
        )
        .content(content)
        // The window bar's Back closes the open issue, and a narrow
        // window's own Back does too.
        .on_back(
            move || back.open.with(Option::is_some) || back.project.with(Option::is_some),
            move || {
                closer.open.set(None);
                closer.project.set(None);
            },
        );
    Box::new(window_bar::TasksWindow::new(
        shell,
        nav.clone(),
        addresses.clone(),
        Box::new(dialog),
    ))
}

/// The sidebar's rows: what narrows the tracker, and the labels it carries.
fn sidebar_sections(tracker: &Tracker, me: Option<&str>) -> Vec<NavSection> {
    let filters = Filter::ALL
        .into_iter()
        .map(|filter| {
            let count = tracker
                .issues
                .values()
                .filter(|issue| filter.matches(issue, me))
                .count();
            NavItem::new(filter.key(), filter.title())
                .icon(filter.icon())
                .count(u32::try_from(count).unwrap_or(u32::MAX))
                .without_actions()
        })
        .collect();
    let mut sections = vec![NavSection::untitled("filters", filters)];
    let labels: Vec<NavItem> = filter::labels(tracker)
        .into_iter()
        .map(|label| {
            let filter = Filter::Label(label.clone());
            let count = tracker
                .issues
                .values()
                .filter(|issue| issue.labels.iter().any(|carried| *carried == label))
                .count();
            NavItem::new(filter.key(), label)
                .icon(ui::icons::Icon::Tag)
                .count(u32::try_from(count).unwrap_or(u32::MAX))
                .without_actions()
        })
        .collect();
    if !labels.is_empty() {
        sections.push(NavSection::untitled("labels", labels));
    }
    let projects: Vec<NavItem> = tracker
        .areas
        .values()
        .map(|area| {
            let held = tracker.in_area(&area.id).count();
            NavItem::new(area_resource(&area.id), area.name.clone())
                .icon(match area.kind {
                    model::Kind::Project => ui::icons::Icon::Folder,
                    model::Kind::Area => ui::icons::Icon::Layers,
                })
                .count(u32::try_from(held).unwrap_or(u32::MAX))
                .without_actions()
        })
        .collect();
    sections.push(
        NavSection::new("projects", tr("tasks-projects"), projects)
            .empty_caption(tr("tasks-project-none")),
    );
    sections
}

/// A row to create an issue: its title, then Enter or the button.
pub(crate) fn new_issue(cx: &ViewCx<Tracker>, parent: Option<Id>, id: &str) -> Element {
    let draft = cx.local(String::new());
    let create_under_parent = parent.is_some();
    // The issue is made from what the field parses to: nothing while it is
    // blank, so the button is disabled and Enter does nothing until there is
    // a title.
    let create = Tracker::CREATE.try_map(move |title: String| {
        Some(crate::model::tracker::Create {
            title: ui::tilo_resource::NonBlank::parse(&title)?,
            parent: parent.clone(),
            body: None,
            status: None,
            assignee: None,
        })
    });
    let title = cx.actions().submit_draft(
        TextInput::from_state(&draft)
            .with_id(&format!("{id}-title"))
            .with_placeholder(if create_under_parent {
                "tasks-new-sub-issue-placeholder"
            } else {
                "tasks-new-issue-placeholder"
            }),
        &draft,
        create.clone(),
    );
    let button = cx.actions().bind_draft(
        Button::new(&format!("{id}-create"), "tasks-new-issue").with_icon(ui::icons::Icon::Plus),
        &draft,
        create,
    );
    Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Container::new()
                    .with_flex_grow()
                    .with_no_padding()
                    .with_child(Box::new(title)),
            ))
            .push(Box::new(button)),
    )
}

/// The project `anchor` names, when it names one: the address it is
/// published under, `tasks.tracker/areas/<id>`.
fn area_of_anchor(anchor: &str) -> Option<Id> {
    let rest = anchor.strip_prefix(crate::model::KIND)?.strip_prefix('/')?;
    let (collection, item) = rest.split_once('/')?;
    (collection == "areas").then(|| Id::from_raw(item))
}

/// How an issue reads in a reference: `#12`.
pub(crate) fn reference(number: u64) -> String {
    crate::model::search::reference(number)
}

ui::dev_reload_entry!(App);

/// The window's addresses: an issue a row opens, a reference it resolves
/// (`#12`), a project by the resource it is published under, and the
/// scenario a capture stages. Returns the tracker as the window reads it.
fn router(cx: &ViewCx<Tracker>, nav: &Nav) -> LocalState<Tracker> {
    let opener = nav.clone();
    cx.on_open_item(move |issue| {
        opener.open(issue);
        Ok(())
    });
    let staged = cx.value(Tracker::clone);
    let (numbered, opener) = (staged.clone(), nav.clone());
    cx.on_open(move |anchor: &str| {
        if let Some(area) = area_of_anchor(anchor) {
            opener.project.set(Some(area));
            opener.open.set(None);
            return Ok::<(), Refusal>(());
        }
        let number = model::search::number(anchor)
            .ok_or_else(|| Refusal::new(format!("`{anchor}` names no issue")))?;
        let tracker = numbered.get();
        let (issue, _) = tracker
            .by_number(number)
            .ok_or_else(|| Refusal::new(format!("There is no issue {anchor}")))?;
        opener.project.set(None);
        opener.open(issue);
        Ok::<(), Refusal>(())
    });
    #[cfg(feature = "samples")]
    {
        let stage = nav.clone();
        let staged_for_stage = staged.clone();
        cx.on_stage(move |scenario| sample::stage(scenario, &stage, &staged_for_stage.get()));
    }
    let asking = nav.asking.clone();
    let open = nav.clone();
    cx.effects(move |effect: TasksEffect, cx: &mut EffectCx<Tracker>| {
        // The "New project" effect is the sidebar's own button, the palette's
        // entry and an agent's call alike: the form it asks for is what this
        // window shows, and the tracker is not touched.
        match effect {
            TasksEffect::NewProject => {
                asking.set(true);
                Ok(Effected::done())
            }
            TasksEffect::OpenIssue { issue } => {
                open.open(&issue);
                Ok(Effected::done())
            }
            // A chat is a window of its own: the desktop opens another
            // window of Tasks beside this one, launched on the issue's chat.
            // A window that already is that chat shows it.
            TasksEffect::OpenChat { issue } => {
                if !cx.live().with(|tracker| tracker.issues.contains(&issue)) {
                    return Err("There is no such issue".to_owned());
                }
                if open.chat.with(|chat| chat.as_ref() != Some(&issue)) {
                    ui::shell_tabs::open_beside(Some(chat::uri(&issue)), false);
                }
                Ok(Effected::done())
            }
            effect @ TasksEffect::WriteNote { .. } => note::handle(effect, cx),
        }
    });
    staged
}

/// The sidebar: what narrows the tracker, the labels in use, and the
/// projects and areas the work goes towards. It is built again as the
/// tracker gains issues, labels or projects.
fn sidebar(cx: &ViewCx<Tracker>, nav: &Nav, tracker: &LocalState<Tracker>) -> Element {
    let chosen = nav.clone();
    let selection = nav.filter.clone();
    let me = cx.me().map(str::to_owned);
    cx.switch(tracker, move |_cx, tracker| {
        let chosen = chosen.clone();
        Box::new(
            ui::widget::NavList::new(sidebar_sections(tracker, me.as_deref()))
                // Named for what it narrows, not the app: the sidebar's
                // heading and the window's tab already say "Tasks".
                .with_accessible_name(tr("tasks-filters"))
                .with_semantic_key("tasks.filters")
                .bind_selection(&selection)
                .on_view("tasks.filters", move |key| {
                    chosen.open.set(None);
                    // A project's row is addressed by the resource it is
                    // published under, and opens that project's page.
                    if let Some(area) = area_of_anchor(&key) {
                        chosen.filter.set(None);
                        chosen.project.set(Some(area));
                    } else {
                        chosen.project.set(None);
                        let filter = Filter::of(&key);
                        // The running tasks are the task list's own, a place
                        // of their own rather than a narrower list: the row
                        // that asks for them shows them.
                        if filter == Some(Filter::Running) {
                            chosen.place.set(Place::Tasks);
                        } else if chosen.place.get() == Place::Tasks {
                            chosen.place.set(Place::List);
                        }
                        chosen.filter.set(filter.map(|filter| filter.key()));
                    }
                }),
        ) as Element
    })
}

ui::reach_tests!(App);
