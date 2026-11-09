//! Console: what your agents do in your workspaces, for a person who mostly
//! has agents work for them rather than working at the desktop themselves.
//!
//! Four places, one beside the other in the sidebar:
//!
//! * **Agents** (`agents`): every run of every agent across the person's
//!   workspaces, live ones first. A run is the timeline of the actions it
//!   took, and every action opens its own view (the file it changed, the
//!   program it ran and what that printed, the page it acted on, the record it
//!   edited): nothing an agent does is opaque. A run can be stopped, undone
//!   whole or one action at a time, or taken over in the full desktop.
//! * **Workspaces** (`workspaces`): the workspaces the person or their agents
//!   made, each one step from the full desktop ("Open in Tilo").
//! * **Connect** (`connect`): the one line that connects an MCP client, and
//!   the agents let in, each revocable.
//! * **Usage** (`usage`): what the agents' machines ran for.
//!
//! Nothing here is the app's own: runs are the workspaces' history, served
//! under [`ui::tilo_actions::remote::workspace_activity`] like Activity's
//! feed; the door and its grants are the agent door's
//! ([`ui::tilo_actions::remote::agent_door`]). The app follows those reads
//! and asks for changes through the same doors; its record holds only which
//! runs Agents shows.
//!
//! The person, a collaborator and an agent never look alike: a person wears a
//! round avatar, an agent a square one with the agent mark, and an agent's
//! line always says whom it acted for.

/// The application id.
pub const APP_ID: &str = "so.tilo.console";
/// The window and launcher title.
pub const APP_TITLE: &str = "Console";

mod agents;
mod connect;
mod effects;
pub mod locales;
mod record;
#[cfg(feature = "samples")]
mod sample;
mod usage;
mod workspaces;

use ui::time::{Duration, SystemTime};

use ui::framework::{Publishes, RecordEnv};
use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::RemoteActionTarget;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{ListRow, NavItem, NavSection};

pub use effects::ConsoleEffect;
pub use record::Console;

/// What the window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// Every agent run, live ones first.
    #[default]
    Agents,
    /// The workspaces the person and their agents made.
    Workspaces,
    /// How an agent connects, and the agents let in.
    Connect,
    /// What the agents' machines ran for.
    Usage,
}

impl Place {
    const ALL: [Self; 4] = [Self::Agents, Self::Workspaces, Self::Connect, Self::Usage];

    const fn key(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Workspaces => "workspaces",
            Self::Connect => "connect",
            Self::Usage => "usage",
        }
    }

    fn of(key: Option<&str>) -> Self {
        Self::ALL
            .into_iter()
            .find(|place| Some(place.key()) == key)
            .unwrap_or_default()
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Agents => "console-place-agents",
            Self::Workspaces => "console-place-workspaces",
            Self::Connect => "console-place-connect",
            Self::Usage => "console-place-usage",
        }
    }

    const fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Agents => ui::icons::Icon::Bot,
            Self::Workspaces => ui::icons::Icon::LayoutGrid,
            Self::Connect => ui::icons::Icon::Link,
            Self::Usage => ui::icons::Icon::Gauge,
        }
    }
}

/// The window's view state, shared by its parts: never stored or synced.
#[derive(Clone)]
pub(crate) struct Nav {
    /// The key of the place shown, as the sidebar selects it.
    pub(crate) place: LocalState<Option<String>>,
    pub(crate) pane: LocalState<ui::widget::NavPane>,
    /// The run Agents shows in place of every run, by its id.
    pub(crate) run: LocalState<Option<String>>,
    /// The action of that run whose view is open, by its place in the run.
    pub(crate) action: LocalState<Option<usize>>,
}

impl Nav {
    pub(crate) fn place(&self) -> Place {
        Place::of(self.place.get().as_deref())
    }

    /// Show `place`, as a capture stages it.
    #[cfg(feature = "samples")]
    pub(crate) fn show(&self, place: Place) {
        self.place.set(Some(place.key().to_owned()));
        self.pane.set(ui::widget::NavPane::Content);
    }

    /// Show the run `id`, no action of it open.
    pub(crate) fn open_run(&self, id: &str) {
        self.action.set(None);
        self.run.set(Some(id.to_owned()));
    }

    /// Show the run `id` from wherever the window is: a run's page is the
    /// Agents place, so opening the run a link names shows it there.
    pub(crate) fn show_run(&self, id: &str) {
        self.place.set(Some(Place::Agents.key().to_owned()));
        self.pane.set(ui::widget::NavPane::Content);
        self.open_run(id);
    }

    /// Show every run again.
    pub(crate) fn close_run(&self) {
        self.action.set(None);
        self.run.set(None);
    }
}

/// What every place reads, followed once for the window: a place shown
/// again, or a run chosen, reads the answers already here rather than asking
/// anew.
#[derive(Clone)]
pub(crate) struct Reads {
    /// Every agent run in every workspace.
    pub(crate) runs: Query<wire::Runs>,
    /// The agent door: its grants and the workspaces it reaches.
    pub(crate) door: Query<door::Door>,
}

impl Reads {
    /// Every run the history listed, once it answered: none before, so a
    /// run's address resolves meanwhile without its run.
    pub(crate) fn runs(&self) -> Vec<wire::AgentRun> {
        self.runs.answer().with(|read| match read {
            Arrival::Ready(runs) => runs.runs.clone(),
            Arrival::Pending { .. } | Arrival::Failed(_) => Vec::new(),
        })
    }
}

/// A door the app reads and asks through: the workspaces' history or the
/// agent door.
#[derive(Clone)]
pub(crate) struct Doors(RecordEnv<Console>);

impl Doors {
    pub(crate) fn of(cx: &ViewCx<Console>) -> Self {
        Self(RecordEnv::clone(cx))
    }

    fn ask(&self, target: RemoteActionTarget, argument: InvokeArgument) {
        self.0.invoke(target, argument);
    }

    /// Ask the history for `action` with `value` as its argument.
    pub(crate) fn history(&self, action: &str, value: &impl serde::Serialize) {
        self.ask(wire::target(action), wire::argument(value));
    }

    /// Ask the agent door for `action` with `value` as its argument.
    pub(crate) fn door(&self, action: &str, value: &impl serde::Serialize) {
        self.ask(door::target(action), door::argument(value));
    }

    /// Follow the history's read `action` with `value` as its argument.
    pub(crate) fn follow_history<T>(&self, action: &str, value: &impl serde::Serialize) -> Query<T>
    where
        T: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        self.0.query(wire::target(action), wire::argument(value))
    }

    /// Follow the agent door.
    pub(crate) fn follow_door(&self) -> Query<door::Door> {
        self.0.query(door::target(door::DOOR), InvokeArgument::None)
    }

    /// Ask for the link to `resource`, as the framework's `share-link` hands
    /// it out: copying it is the framework's, so a link to a run is the same
    /// link any resource gets.
    pub(crate) fn share_link(&self, resource: &str) {
        self.0.dispatcher().share_link(resource);
    }

    /// Open the full desktop where `href`, a `tilo://open#…` address, says.
    pub(crate) fn open_in_tilo(&self, href: &str) {
        // The address is the door's, and an open the session refuses is
        // told where the open lands; one that does not parse names nothing
        // to open.
        let _ = self.0.dispatcher().open_link(href);
    }
}

pub(crate) fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

pub(crate) fn trf(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    I18nManager::format(key, args)
}

/// When `at_ms` was, as "5 minutes ago".
pub(crate) fn ago(at_ms: i64) -> String {
    let at = u64::try_from(at_ms)
        .ok()
        .map_or(SystemTime::UNIX_EPOCH, |ms| {
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_millis(ms))
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
    ui::format_relative_time(at, ui::time::wall())
}

/// Who `author` is, as a line names them: "You", a person's name, or an
/// agent with whom it acted for.
pub(crate) fn who(author: &wire::Author) -> String {
    match (author.relation, &author.on_behalf_of) {
        (wire::Relation::Reader, _) => tr("console-you"),
        (wire::Relation::Software, Some(person)) => trf(
            "console-agent-for",
            &[
                ("agent", author.name.clone().into()),
                ("person", person.clone().into()),
            ],
        ),
        _ => author.name.clone(),
    }
}

/// A row led by `author`'s face: a person's round avatar, or an agent's
/// square one with the agent mark, so the two are never mistaken.
pub(crate) fn faced(row: ListRow, author: &wire::Author) -> ListRow {
    let row = row.avatar(author.name.clone(), author.identity.clone());
    match author.relation {
        wire::Relation::Software => row.square().avatar_glyph(ui::icons::Icon::Bot),
        wire::Relation::Reader | wire::Relation::Person => row,
    }
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
    type Resource = Console;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut Fixture<Console>) {
        sample::fixture(fixture);
    }

    fn view(cx: &mut ViewCx<Console>) -> Element {
        let nav = Nav {
            place: cx.local(Some(Place::Agents.key().to_owned())),
            pane: cx.local(ui::widget::NavPane::Content),
            run: cx.local(None),
            action: cx.local(None),
        };
        #[cfg(feature = "samples")]
        {
            let stage = nav.clone();
            cx.on_stage(move |scenario| sample::stage(scenario, &stage));
        }
        let doors = Doors::of(cx);
        let reads = Reads {
            runs: doors.follow_history(wire::RUNS, &agents::query()),
            door: doors.follow_door(),
        };
        // Opening the run a link names shows its page here, in this window.
        let opened = nav.clone();
        cx.effects(move |effect: ConsoleEffect, _cx| {
            match effect {
                ConsoleEffect::ViewRun { run } => opened.show_run(&run),
            }
            Ok(ui::framework::Effected::done())
        });
        let (listed, named) = (reads.clone(), reads.clone());
        let shown = nav.clone();
        let content = cx.switch(&nav.place, move |cx, _| {
            let (nav, reads) = (shown.clone(), reads.clone());
            cx.switch(&shown.run, move |cx, _| window(cx, &nav, &reads))
        });
        cx.publishing(
            Publishes::new()
                // Each run shown is published as the resource its link names,
                // and so is any run whose address is asked for before the
                // history's read lands: a link opens straight into a fresh
                // window, which shows the run once its read answers.
                .live_rows(move || agents::live_runs(&listed.runs()))
                .live_row(move |resource| agents::live_run_at(&named.runs(), resource)),
            content,
        )
    }
}

/// The window as it shows `nav`'s place: the sidebar of places, and the place
/// with its commands in the header.
fn window(cx: &ViewCx<Console>, nav: &Nav, reads: &Reads) -> Element {
    let chosen = nav.clone();
    let sidebar = ui::widget::NavList::new(vec![NavSection::untitled(
        "places",
        Place::ALL
            .into_iter()
            .map(|place| NavItem::new(place.key(), tr(place.label())).icon(place.icon()))
            .collect(),
    )])
    .with_accessible_name(tr("console-places"))
    .with_semantic_key("console.places")
    .bind_selection(&nav.place)
    .on_view("console.places", move |_| {
        chosen.pane.set(ui::widget::NavPane::Content);
    });

    let place = nav.place();
    let shell = ui::widget::NavShell::new()
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("console.shell")
        .mode(ui::widget::NavMode::Push)
        .with_pane_state(&nav.pane)
        .sidebar(Box::new(sidebar))
        .sidebar_header(tr("console-title"), Vec::new());
    Box::new(match place {
        Place::Agents => {
            let (content, actions) = agents::view(cx, nav, reads);
            shell
                .content(content)
                .content_header(tr(place.label()), actions)
        }
        Place::Workspaces => shell
            .content(workspaces::view(cx, reads))
            .content_header(tr(place.label()), Vec::new()),
        Place::Connect => shell
            .content(connect::view(cx, reads))
            .content_header(tr(place.label()), Vec::new()),
        Place::Usage => shell
            .content(usage::view())
            .content_header(tr(place.label()), Vec::new()),
    })
}

ui::dev_reload_entry!(App);

ui::reach_tests!(App);
