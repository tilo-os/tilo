//! Tasks: the workspace's issues, on a list and a board.
//!
//! The tracker is the app's resource of the workspace ([`model`]): the
//! projects and areas above its issues. The issues — nested, each with a
//! markdown description, a status, an assignee, labels, links and a thread —
//! are kept in numbered parts beside it ([`part`]), so a change to one costs
//! its part whatever the tracker holds. The list nests sub-issues under their parent; the board has
//! a list per status, and dragging a card is one `place` action; the issue
//! page edits the description in the shared block editor and holds the
//! thread. An issue assigned to an agent is that agent's: it works on the
//! issue by the tracker's own actions, from the conversation it was asked in.
//! An issue's note is written in Notes, through Notes' own action (`note`).
//!
//! Storage, sync, history, undo, search and permissions are the framework's.
//!
//! The sidebar is the tracker read the way a person works through it: every
//! issue, the ones assigned to them, the ones waiting to be started, the ones
//! nobody has taken, the ones an agent has, one row per label in use, and the
//! projects and areas
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
mod filter;
mod import_inbox;
mod issue;
pub mod issue_list;
mod list;
pub mod locales;
mod program_step;
mod project;
#[cfg(feature = "samples")]
mod sample;
mod thread;
mod window_bar;

pub(crate) use filter::Filter;

pub use tasks_model as model;

use std::rc::Rc;

use ui::framework::{Actions, Chain, EffectCx, Effected, Parts};
use ui::prelude::*;
use ui::widget::{NavItem, NavSection};

pub use issue_list::{IssueList, Issues};
pub use model::Tracker;
pub use model::{NOTES, TasksEffect};
use model::{Status, area_resource};

/// What the window reads: the tracker's projects and the parts its issues
/// are kept in, as they stand.
#[derive(Clone, Default)]
pub(crate) struct Tasks {
    pub(crate) tracker: Rc<Tracker>,
    pub(crate) parts: Parts<IssueList>,
    /// The issues read across the parts, kept for every version of the
    /// window's state: each reads it forward from what changed
    /// ([`Issues::sync`]), never anew.
    index: Rc<std::cell::RefCell<Issues>>,
}

impl PartialEq for Tasks {
    fn eq(&self, other: &Self) -> bool {
        self.tracker == other.tracker && Rc::ptr_eq(&self.parts, &other.parts)
    }
}

impl Tasks {
    /// Every issue the parts keep, read as one tracker.
    pub(crate) fn issues(&self) -> IssuesRead<'_> {
        read_issues(&self.index, &self.parts)
    }
}

/// The issues `parts` keep, read through `index`, the window's kept reading of them: brought
/// forward to `parts` from what changed, never read anew.
pub(crate) fn read_issues<'a>(
    index: &'a std::cell::RefCell<Issues>,
    parts: &Parts<IssueList>,
) -> IssuesRead<'a> {
    let behind = index.try_borrow().is_ok_and(|index| !index.follows(parts));
    if behind && let Ok(mut index) = index.try_borrow_mut() {
        index.sync(parts);
    }
    match index.try_borrow() {
        Ok(index) if index.follows(parts) => IssuesRead::Kept(index),
        // Read while another version's read is held: read these parts on their own.
        _ => {
            let _profile = ui::tilo_resource::profiling::Phase::start("tasks.issues_read_whole");
            IssuesRead::Own(Box::new(Issues::of(parts)))
        }
    }
}

/// The issues as a version of the window's state reads them ([`Tasks::issues`]).
pub(crate) enum IssuesRead<'a> {
    Kept(std::cell::Ref<'a, Issues>),
    Own(Box<Issues>),
}

impl std::ops::Deref for IssuesRead<'_> {
    type Target = Issues;

    fn deref(&self) -> &Issues {
        match self {
            Self::Kept(index) => index,
            Self::Own(index) => index,
        }
    }
}

/// The window's hold on the issues: what it reads of them, and the chain
/// their actions are carried out through. Shared by every part of the view.
#[derive(Clone)]
pub(crate) struct Window {
    pub(crate) tasks: LocalState<Tasks>,
    /// The issues as the window reads them, shared by every version of `tasks`.
    pub(crate) index: Rc<std::cell::RefCell<Issues>>,
    pub(crate) chain: Option<Chain<IssueList>>,
    /// The chain's published actions, for the controls bound to them.
    pub(crate) actions: Option<Actions<IssueList>>,
}

impl Window {
    fn of(cx: &ViewCx<Tracker>) -> Self {
        let chain = cx.chain::<IssueList>();
        let index = Rc::new(std::cell::RefCell::new(Issues::default()));
        let kept = Rc::clone(&index);
        let tasks = match &chain {
            Some(chain) => {
                cx.chain_value(chain, move |tracker: &Tracker, parts: &Parts<IssueList>| {
                    Tasks {
                        tracker: Rc::new(tracker.clone()),
                        parts: Rc::clone(parts),
                        index: Rc::clone(&kept),
                    }
                })
            }
            // A past version shows the tracker's projects and no issues:
            // the parts are not part of the resource it shows.
            None => cx.value(|tracker: &Tracker| Tasks {
                tracker: Rc::new(tracker.clone()),
                parts: Parts::<IssueList>::default(),
                index: Rc::default(),
            }),
        };
        let actions = chain.as_ref().and_then(|chain| cx.resource_actions(chain));
        Self {
            tasks,
            index,
            chain,
            actions,
        }
    }

    /// Carry `invoke` out on the issues, as the person: a refusal is told
    /// where the window tells its person what was refused.
    pub(crate) fn submit(&self, invoke: ui::tilo_resource::Invoke<IssueList>) {
        if let Some(chain) = &self.chain {
            chain.dispatcher().submit(invoke);
        }
    }
}

/// What the window shows when no issue is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// Every issue, sub-issues under their parent.
    #[default]
    List,
    /// A list per status.
    Board,
    /// The tasks that have said something, as their conversations.
    Thread,
}

impl Place {
    /// The places the window shows, in the bar's order.
    pub(crate) const ALL: [Self; 3] = [Self::List, Self::Board, Self::Thread];

    /// The place as the window bar and the bar's view switcher name it.
    pub(crate) fn title(self) -> String {
        match self {
            Self::List => tr("tasks-place-list"),
            Self::Board => tr("tasks-place-board"),
            Self::Thread => tr("tasks-place-thread"),
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
    /// The issue a press in this window just made, by its number: the list shows it.
    pub(crate) made: LocalState<Option<u64>>,
    /// The "New project" dialog is asking.
    pub(crate) asking: LocalState<bool>,
    /// The new project's name, while the dialog asks.
    pub(crate) named: LocalState<String>,
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
    crate::locales::CATALOG.t(key)
}

/// A message with named text arguments.
pub(crate) fn tr_with(key: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, ui::i18n::ArgValue::Text((*value).to_string())))
        .collect();
    crate::locales::CATALOG.t_args(key, &args)
}

pub struct App;

impl ResourceApp for App {
    type Resource = Tracker;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const TITLE_KEY: &'static str = "tasks-app-title";
    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    /// The issues, kept in parts beside the tracker.
    fn resources(resources: &mut ui::framework::Resources<'_>) {
        resources.chain::<IssueList>();
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1240, 780)
    }

    /// Issues a migration brought from Linear join the tracker when it opens.
    fn import_resource(context: &ui::application::ProcessContext) -> Result<(), String> {
        crate::import_inbox::import(context)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    /// A scenario's issues, made as the window opens, and its views after.
    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut ui::framework::Fixture<Tracker>) {
        match fixture.scenario() {
            Some("migrated" | "migrated-issue") => fixture.provide(sample::Scene::Migrated),
            Some(long) if let Some(scene) = sample::Scene::long(long) => fixture.provide(scene),
            Some(_) => fixture.provide(sample::Scene::Launch),
            None => {}
        }
    }

    fn view(cx: &mut ViewCx<Tracker>) -> Element {
        let (asking, named) = (cx.local(false), cx.local(String::new()));
        let nav = Nav {
            place: cx.local(Place::List),
            open: cx.local(None),
            filter: cx.local(None),
            project: cx.local(None),
            made: cx.local(None),
            asking,
            named,
        };
        let window = Window::of(cx);
        #[cfg(feature = "samples")]
        if let (Some(scene), Some(chain), Some(me)) = (
            cx.fixture::<sample::Scene>(),
            &window.chain,
            cx.me().and_then(ui::tilo_resource::MemberId::person_id),
        ) {
            sample::fill(*scene, chain, me);
        }
        router(cx, &nav, &window);
        let shown = nav.clone();
        let tasks = window.tasks.clone();
        let me = cx.me().cloned();
        let landed = cx.dispatcher().landed();
        cx.upkeep(
            ui::framework::Upkeep::new()
                .syncing(move || made_issue_landed(&shown, &tasks, me.as_ref(), &landed)),
            tracker_window(cx, &nav, &window),
        )
    }
}

/// Show the issue a press in this window made: the list scrolls to it and marks it chosen.
/// Only the list's own field makes one this way, where the window is on the tracker's
/// issues — the board and an issue's page make their issues where they stand, and show
/// them there.
fn made_issue_landed(
    nav: &Nav,
    tasks: &LocalState<Tasks>,
    me: Option<&ui::tilo_resource::MemberId>,
    landed: &LocalState<Option<ui::framework::Landed>>,
) {
    let Some(done) = landed.get() else {
        return;
    };
    if done.action != IssueList::CREATE.id() {
        return;
    }
    let made = done
        .created
        .iter()
        .find_map(|address| ui::tilo_resource::items::item_of_resource::<IssueList>(address))
        .and_then(|(_, issue)| tasks.with(|tasks| tasks.issues().get(&issue).cloned()));
    let Some(held) = made else {
        // The parts have not read the issue yet: it is shown once they have.
        return;
    };
    landed.set(None);
    // Where the press was made shows it: the board and an issue's page show their own.
    if nav.place.with(|place| *place != Place::List)
        || nav.open.with(Option::is_some)
        || nav.project.with(Option::is_some)
    {
        return;
    }
    // The sidebar's filters narrow the list: an issue the filter leaves out is shown by
    // showing every issue, which is where the press made it.
    if !nav.filter().matches(&held, me) {
        nav.filter.set(None);
    }
    nav.made.set(Some(held.number));
}

/// The window over the whole tracker: the sidebar, the list, the board and
/// the issue pages.
fn tracker_window(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) -> Element {
    let sidebar = sidebar(cx, nav, window);
    let content_nav = nav.clone();
    let shown = window.clone();
    let content = cx.switch(&nav.open, move |cx, open| {
        if let Some(issue) = open {
            return issue::page(cx, issue.clone(), &content_nav, &shown);
        }
        let nav = content_nav.clone();
        let window = shown.clone();
        cx.switch(&content_nav.project, move |cx, project| {
            if let Some(project) = project {
                return project::page(cx, project.clone(), &window);
            }
            let nav = nav.clone();
            let content_nav = nav.clone();
            let window = window.clone();
            cx.switch(&content_nav.place, move |cx, place| match place {
                Place::List => list::view(cx, &nav, &window),
                Place::Board => board::view(cx, &nav, &window),
                Place::Thread => thread::view(cx, &nav, &window),
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
        cx.toolbar(),
        nav.clone(),
        window.tasks.clone(),
        window.chain.clone(),
        Box::new(dialog),
    ))
}

/// The sidebar's rows: what narrows the tracker, and the labels it carries.
fn sidebar_sections(
    tasks: &Tasks,
    me: Option<&ui::tilo_resource::MemberId>,
    loading: bool,
) -> Vec<NavSection> {
    let issues = tasks.issues();
    // What a row counts: nothing while parts of the tracker are still on their way.
    let counted = |count: usize| {
        if loading {
            0
        } else {
            u32::try_from(count).unwrap_or(u32::MAX)
        }
    };
    let filters = Filter::ALL
        .into_iter()
        .map(|filter| {
            let count = filter.count(&issues, me);
            NavItem::new(filter.key(), filter.title())
                .icon(filter.icon())
                .count(counted(count))
                .without_actions()
        })
        .collect();
    let mut sections = vec![NavSection::untitled("filters", filters)];
    // Each label's count is the tally the issues keep, not a walk of every issue per label.
    let labels: Vec<NavItem> = issues
        .labels()
        .map(|(label, count)| {
            let filter = Filter::Label(label.to_owned());
            NavItem::new(filter.key(), label.to_owned())
                .icon(ui::icons::Icon::Tag)
                .count(counted(count))
                .without_actions()
        })
        .collect();
    if !labels.is_empty() {
        sections.push(NavSection::untitled("labels", labels));
    }
    let projects: Vec<NavItem> = tasks
        .tracker
        .areas
        .values()
        .map(|area| {
            let held = issues.in_area(&area.id).count();
            NavItem::new(area_resource(&area.id), area.name.clone())
                .icon(match area.kind {
                    model::Kind::Project => ui::icons::Icon::Folder,
                    model::Kind::Area => ui::icons::Icon::Layers,
                })
                .count(counted(held))
                .without_actions()
        })
        .collect();
    sections.push(
        NavSection::new("projects", tr("tasks-projects"), projects)
            .empty_caption(tr("tasks-project-none")),
    );
    sections
}

/// A row to create an issue: its title, then Enter or the button. `status`
/// is where it starts, when the row stands in a list of the board; `local`
/// makes the title's draft.
pub(crate) fn new_issue(
    local: &dyn Fn() -> ui::LocalState<String>,
    window: &Window,
    parent: Option<Id>,
    status: Option<Status>,
    id: &str,
) -> Element {
    let Some(actions) = &window.actions else {
        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
    };
    let draft = local();
    let create_under_parent = parent.is_some();
    // The issue is made from what the field parses to: nothing while it is
    // blank, so the button is disabled and Enter does nothing until there is
    // a title.
    let create = IssueList::CREATE.try_map(move |title: String| {
        Some(crate::model::NewIssue {
            parent: parent.as_ref().map(ui::tilo_resource::items::Ref::from),
            status,
            ..crate::model::NewIssue::titled(ui::tilo_resource::NonBlank::parse(&title)?)
        })
    });
    let title = actions.submit_draft(
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
    let button = actions.bind_draft(
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
/// scenario a capture stages.
fn router(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) {
    let opener = nav.clone();
    cx.on_open_item(move |issue| {
        opener.open(issue);
        Ok(())
    });
    let (numbered, opener) = (window.tasks.clone(), nav.clone());
    cx.on_open(move |anchor: &str| {
        // The tracker itself, by the resource its own documents are published
        // under: the window shows its issues, which is what it is.
        if anchor.trim() == crate::model::KIND {
            opener.project.set(None);
            opener.open.set(None);
            return Ok::<(), Refusal>(());
        }
        // An issue, by the address its part publishes it under.
        if let Some((_, issue)) = ui::tilo_resource::items::item_of_resource::<IssueList>(anchor) {
            opener.project.set(None);
            opener.open(&issue);
            return Ok::<(), Refusal>(());
        }
        if let Some(area) = area_of_anchor(anchor) {
            opener.project.set(Some(area));
            opener.open.set(None);
            return Ok::<(), Refusal>(());
        }
        let number = model::search::number(anchor)
            .ok_or_else(|| Refusal::new(format!("`{anchor}` names no issue")))?;
        let tasks = numbered.get();
        let issue = tasks
            .issues()
            .by_number(number)
            .map(|(issue, _)| issue.clone())
            .ok_or_else(|| Refusal::new(format!("There is no issue {anchor}")))?;
        opener.project.set(None);
        opener.open(&issue);
        Ok::<(), Refusal>(())
    });
    #[cfg(feature = "samples")]
    {
        let (stage, staged) = (nav.clone(), window.tasks.clone());
        cx.on_stage(move |scenario| sample::stage(scenario, &stage, &staged.get()));
    }
    let asking = nav.asking.clone();
    let open = nav.clone();
    cx.effects(move |effect: TasksEffect, _cx: &mut EffectCx<Tracker>| {
        // The "New project" effect is the sidebar's own button and the
        // palette's entry: the form it asks for is what this window shows,
        // and the tracker is not touched. Only the person at this screen opens
        // it; an agent makes a project with the tracker's `create-area`.
        match effect {
            TasksEffect::NewProject => {
                asking.set(true);
                Ok(Effected::done())
            }
            TasksEffect::OpenIssue { issue } => {
                open.open(&issue);
                Ok(Effected::done())
            }
            // The places are the window's views: choosing one shows it, in
            // the bar or from the palette or an agent alike.
            place @ (TasksEffect::ShowList | TasksEffect::ShowBoard | TasksEffect::ShowThread) => {
                open.place.set(match place {
                    TasksEffect::ShowBoard => Place::Board,
                    TasksEffect::ShowThread => Place::Thread,
                    _ => Place::List,
                });
                open.open.set(None);
                Ok(Effected::done())
            }
        }
    });
}

/// The sidebar: what narrows the tracker, the labels in use, and the
/// projects and areas the work goes towards. It is built again as the
/// tracker gains issues, labels or projects.
fn sidebar(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) -> Element {
    let chosen = nav.clone();
    let selection = nav.filter.clone();
    let me = cx.me().cloned();
    let chain = window.chain.clone();
    cx.switch(&window.tasks, move |_cx, tasks| {
        let chosen = chosen.clone();
        // A count counts the issues this device holds. While parts of the tracker are still on
        // their way, that is fewer than the tracker has, so the sidebar shows no counts rather
        // than a number that is about to grow.
        let loading = chain.as_ref().is_some_and(|chain| {
            chain
                .waited()
                .iter()
                .any(|(_, arrival)| *arrival == ui::framework::PartArrival::Arriving)
        });
        Box::new(
            ui::widget::NavList::new(sidebar_sections(tasks, me.as_ref(), loading))
                // Named for what it narrows, not the app: the sidebar's
                // heading and the window's tab already say "Tasks".
                .with_accessible_name(tr("tasks-filters"))
                .with_semantic_key("tasks.filters")
                .bind_selection(&selection)
                .on_choose("tasks.filters", move |key| {
                    chosen.open.set(None);
                    // A project's row is addressed by the resource it is
                    // published under, and opens that project's page.
                    if let Some(area) = area_of_anchor(&key) {
                        chosen.filter.set(None);
                        chosen.project.set(Some(area));
                    } else {
                        chosen.project.set(None);
                        chosen
                            .filter
                            .set(Filter::of(&key).map(|filter| filter.key()));
                    }
                }),
        ) as Element
    })
}

ui::reach_tests!(App);
