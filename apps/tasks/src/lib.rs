//! Tasks: the workspace's issues, on a list and a board.
//!
//! The tracker is the app's record of the workspace ([`model`]): nested
//! issues with a markdown description, a status, an assignee, labels, links
//! and a thread. The list nests sub-issues under their parent; the board has
//! a lane per status, and dragging a card is one `place` action; the issue
//! page edits the description in the shared block editor and holds the
//! thread. An issue assigned to an agent is that agent's task: its runs are
//! the issue's actions, approved, steered and answered from the issue page.
//! A private issue is kept in a tracker of its own that only its readers
//! read ([`model::private`]); for them it shows among the others, on the
//! list, the board and in search, and its page edits its own record.
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
    /// Every issue this person reads, private ones among the others.
    pub(crate) shown: LocalState<Shown>,
    /// The private issues' trackers, in a window someone uses.
    pub(crate) private: Option<ui::framework::Children<Tracker>>,
}

impl Nav {
    pub(crate) fn open(&self, issue: &Id) {
        self.open.set(Some(issue.clone()));
    }

    /// The private tracker `issue` is kept in, when it is private.
    pub(crate) fn home(&self, issue: &Id) -> Option<Id> {
        self.shown.with(|shown| shown.private.get(issue).cloned())
    }

    /// Submit `invoke` about `issue` to the tracker that keeps it: its
    /// private tracker's door, or `workspace`'s.
    pub(crate) fn submit(
        &self,
        workspace: &ui::framework::Dispatcher<Tracker>,
        issue: &Id,
        invoke: Invoke<Tracker>,
    ) {
        match self
            .home(issue)
            .zip(self.private.as_ref())
            .and_then(|(home, private)| private.dispatcher(&home))
        {
            Some(dispatcher) => dispatcher.submit(invoke),
            None => workspace.submit(invoke),
        }
    }
}

/// The tracker as this person reads it: the workspace's issues and those of
/// every private tracker they read, in one, and which private tracker keeps
/// each private one.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Shown {
    pub(crate) tracker: Tracker,
    pub(crate) private: std::collections::BTreeMap<Id, Id>,
}

impl Shown {
    fn of(tracker: &Tracker, private: &[(Id, std::rc::Rc<Tracker>)]) -> Self {
        let mut shown = Self {
            tracker: tracker.clone(),
            private: std::collections::BTreeMap::new(),
        };
        for (home, kept) in private {
            for (id, issue) in kept.issues.iter() {
                shown.tracker.issues.push(id.clone(), issue.clone());
                shown.private.insert(id.clone(), home.clone());
            }
        }
        shown
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

    fn records(records: &mut ui::framework::Records<'_>) {
        records.children::<Tracker>();
    }

    fn view(cx: &mut ViewCx<Tracker>) -> Element {
        let private = cx.children::<Tracker>();
        let shown = match &private {
            Some(private) => cx.value_with_children(private, Shown::of),
            None => cx.value(|tracker: &Tracker| Shown::of(tracker, &[])),
        };
        let nav = Nav {
            place: cx.local(Place::List),
            open: cx.local(None),
            shown,
            private,
        };
        let opener = nav.clone();
        cx.on_open_item(move |issue| {
            opener.open(issue);
            Ok(())
        });
        let staged = cx.value(Tracker::clone);
        // A reference as it reads (`#12`) is the tracker's own to resolve:
        // anyone opens the issue it names through the tracker's `open`.
        let opener = nav.clone();
        cx.on_open(move |anchor: &str| {
            let number = model::search::number(anchor)
                .ok_or_else(|| Refusal::new(format!("`{anchor}` names no issue")))?;
            let tracker = opener.shown.get().tracker;
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
        .with_accessible_name(tr("tasks-title"))
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
                return issue::open(cx, issue, &content_nav);
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
