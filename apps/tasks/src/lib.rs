//! Tasks: the workspace's issues, on a list and a board.
//!
//! The tracker is the app's record of the workspace ([`model`]): nested
//! issues with a markdown description, a status, an assignee, labels, links
//! and a thread. The list nests sub-issues under their parent; the board has
//! a lane per status, and dragging a card is one `place` action; the issue
//! page edits the description in the shared block editor and holds the
//! thread. An issue assigned to an agent is that agent's task: its runs are
//! the issue's actions, approved, steered and answered from the issue page.
//!
//! Storage, sync, history, undo, search and permissions are the framework's.
//!
//! The sidebar is the tracker read the way a person works through it: every
//! issue, the ones assigned to them, the ones nobody has taken, the ones an
//! agent is working on, and one row per label in use. The window bar holds
//! the two places (Issues, Board) as its views and names the open issue as
//! its location, so the window draws no title row and no Back of its own.

/// The application id.
pub const APP_ID: &str = crate::model::APP_ID;
/// The window and launcher title.
pub const APP_TITLE: &str = "Tasks";

mod board;
mod filter;
mod issue;
mod list;
pub mod locales;
pub mod model;
mod sample;
mod window_bar;

pub(crate) use filter::Filter;

use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::widget::{NavItem, NavSection};

pub use model::Tracker;

/// What the window shows when no issue is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// Every issue, sub-issues under their parent.
    #[default]
    List,
    /// A lane per status.
    Board,
}

impl Place {
    /// The places the window shows, in the bar's order.
    pub(crate) const ALL: [Self; 2] = [Self::List, Self::Board];

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Board => "board",
        }
    }

    /// The place a bar view's key names, if it names one.
    pub(crate) fn of(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|place| place.key() == key)
    }

    /// The place as the window bar and the bar's view switcher name it.
    pub(crate) fn title(self) -> String {
        match self {
            Self::List => tr("tasks-place-list"),
            Self::Board => tr("tasks-place-board"),
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
}

impl Nav {
    pub(crate) fn open(&self, issue: &Id) {
        self.open.set(Some(issue.clone()));
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

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1240, 780)
    }

    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    /// A capture cannot make six issues and label them: the scenario stages
    /// the tracker it reached as the record's start, and its views after.
    fn fixture(fixture: &mut ui::framework::Fixture<Tracker>) {
        let Some(scenario) = fixture.scenario() else {
            return;
        };
        let _ = scenario;
        fixture.start(sample::tracker(fixture.person()));
    }

    fn view(cx: &mut ViewCx<Tracker>) -> Element {
        let nav = Nav {
            place: cx.local(Place::List),
            open: cx.local(None),
            filter: cx.local(None),
        };
        let opener = nav.clone();
        cx.on_open_item(move |issue| {
            opener.open(issue);
            Ok(())
        });
        let staged = cx.value(Tracker::clone);
        // A reference as it reads (`#12`) is the tracker's own to resolve:
        // anyone opens the issue it names through the tracker's `open`.
        let (numbered, opener) = (staged.clone(), nav.clone());
        cx.on_open(move |anchor: &str| {
            let number = model::search::number(anchor)
                .ok_or_else(|| Refusal::new(format!("`{anchor}` names no issue")))?;
            let tracker = numbered.get();
            let (issue, _) = tracker
                .by_number(number)
                .ok_or_else(|| Refusal::new(format!("There is no issue {anchor}")))?;
            opener.open(issue);
            Ok::<(), Refusal>(())
        });
        let stage = nav.clone();
        let staged_for_stage = staged.clone();
        cx.on_stage(move |scenario| sample::stage(scenario, &stage, &staged_for_stage.get()));

        // The sidebar narrows the tracker, and is built again as the
        // tracker gains issues and labels; the views switch its places.
        let chosen = nav.clone();
        let selection = nav.filter.clone();
        let me = cx.me().map(str::to_owned);
        let sidebar = cx.switch(&staged, move |_cx, tracker| {
            let chosen = chosen.clone();
            Box::new(
                ui::widget::NavList::new(sidebar_sections(tracker, me.as_deref()))
                    .with_id("tasks.filters")
                    // Named for what it narrows, not the app: the sidebar's
                    // heading and the window's tab already say "Tasks".
                    .with_accessible_name(tr("tasks-filters"))
                    .with_semantic_key("tasks.filters")
                    .bind_selection(&selection)
                    .on_select(move |key| {
                        chosen.open.set(None);
                        chosen
                            .filter
                            .set(Filter::of(&key).map(|filter| filter.key()));
                    }),
            ) as Element
        });

        let content_nav = nav.clone();
        let content = cx.switch(&nav.open, move |cx, open| {
            if let Some(issue) = open {
                return issue::page(cx, issue.clone(), &content_nav);
            }
            let nav = content_nav.clone();
            cx.switch(&content_nav.place, move |cx, place| match place {
                Place::List => list::view(cx, &nav),
                Place::Board => board::view(cx, &nav),
            })
        });

        let (back, closer) = (nav.clone(), nav.clone());
        let shell = ui::widget::NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("tasks.shell")
            .mode(ui::widget::NavMode::Push)
            .with_window_toolbar()
            .with_pane_state(&cx.local(ui::widget::NavPane::Content))
            .sidebar(sidebar)
            .sidebar_header(tr("tasks-title"), Vec::new())
            .content(content)
            // The window bar's Back closes the open issue, and a narrow
            // window's own Back does too.
            .on_back(
                move || back.open.with(Option::is_some),
                move || closer.open.set(None),
            );
        Box::new(window_bar::TasksWindow::new(shell, nav, staged))
    }
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
    sections
}

/// A row to create an issue: its title, then Enter or the button.
pub(crate) fn new_issue(cx: &ViewCx<Tracker>, parent: Option<Id>, id: &str) -> Element {
    let draft = cx.local(String::new());
    let title = TextInput::from_state(&draft)
        .with_id(&format!("{id}-title"))
        .with_placeholder(if parent.is_some() {
            "tasks-new-sub-issue-placeholder"
        } else {
            "tasks-new-issue-placeholder"
        });
    let create = Tracker::CREATE.map(move |()| {
        let title = draft.get();
        draft.set(String::new());
        crate::model::tracker::Create {
            title,
            parent: parent.clone(),
            body: None,
            status: None,
            assignee: None,
        }
    });
    let button = cx.actions().bind(
        Button::new(&format!("{id}-create"), "tasks-new-issue").with_icon(ui::icons::Icon::Plus),
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

/// How an issue reads in a reference: `#12`.
pub(crate) fn reference(number: u64) -> String {
    crate::model::search::reference(number)
}

ui::dev_reload_entry!(App);

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::App::scenarios() {
            let mut app = TestApp::<super::App>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
