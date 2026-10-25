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

/// The application id.
pub const APP_ID: &str = crate::model::APP_ID;
/// The window and launcher title.
pub const APP_TITLE: &str = "Tasks";

mod board;
mod issue;
mod list;
pub mod locales;
pub mod model;
mod sample;

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
    const fn key(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Board => "board",
        }
    }
}

/// The window's view state, shared by its parts: never stored or synced.
#[derive(Clone)]
pub(crate) struct Nav {
    pub(crate) place: LocalState<Place>,
    pub(crate) open: LocalState<Option<Id>>,
}

impl Nav {
    pub(crate) fn open(&self, issue: &Id) {
        self.open.set(Some(issue.clone()));
    }
}

pub(crate) fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
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

    fn scenario(name: Option<&str>) -> Vec<Invoke<Tracker>> {
        sample::scenario(name)
    }

    fn view(cx: &mut ViewCx<Tracker>) -> Element {
        let nav = Nav {
            place: cx.local(Place::List),
            open: cx.local(None),
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
        cx.on_stage(move |scenario| sample::stage(scenario, &stage, &staged.get()));

        let selection = cx.local(Some(Place::List.key().to_owned()));
        let chosen = nav.clone();
        let sidebar = ui::widget::NavList::new(vec![NavSection::untitled(
            "places",
            vec![
                NavItem::new(Place::List.key(), tr("tasks-place-list"))
                    .icon(ui::icons::Icon::ListTree),
                NavItem::new(Place::Board.key(), tr("tasks-place-board"))
                    .icon(ui::icons::Icon::LayoutGrid),
            ],
        )])
        .with_id("tasks.places")
        // Named for what it lists, not the app: the sidebar's heading and
        // the window's tab already say "Tasks".
        .with_accessible_name(tr("tasks-places"))
        .with_semantic_key("tasks.places")
        .bind_selection(&selection)
        .on_select(move |key| {
            chosen.open.set(None);
            chosen.place.set(if key == Place::Board.key() {
                Place::Board
            } else {
                Place::List
            });
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

        Box::new(
            ui::widget::NavShell::new()
                .content_kind(ui::widget::ContentKind::Page)
                .with_id("tasks.shell")
                .mode(ui::widget::NavMode::Push)
                .with_pane_state(&cx.local(ui::widget::NavPane::Content))
                .sidebar(Box::new(sidebar))
                .sidebar_header(tr("tasks-title"), Vec::new())
                .content(content),
        )
    }
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
