//! Console: what your agents do in your workspaces, for a person who mostly has agents work
//! for them rather than working at the desktop themselves.

pub use ui::apps::CONSOLE as APP_ID;
pub const APP_TITLE: &str = "Console";

mod agent_card;
mod agents;
mod client;
mod connect;
mod console;
mod effects;
pub mod locales;
#[cfg(feature = "samples")]
mod sample;
mod usage;
mod window;
mod workspaces;

use ui::time::{Duration, SystemTime};

use ui::framework::{Publishes, ResourceEnv};
use ui::prelude::*;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::RemoteActionTarget;
use ui::tilo_actions::remote::mcp_server;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{ListRow, NavItem, NavSection};

pub use console::Console;
pub use effects::ConsoleEffect;

/// What the window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    #[default]
    Agents,
    Workspaces,
    Connect,
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
    pub(crate) place: LocalState<Option<String>>,
    pub(crate) pane: LocalState<ui::widget::NavPane>,
    pub(crate) session: LocalState<Option<String>>,
    pub(crate) action: LocalState<Option<usize>>,
    pub(crate) full_output: LocalState<Option<usize>>,
    pub(crate) agent: LocalState<Option<String>>,
    pub(crate) command: LocalState<Option<effects::AgentCommand>>,
}

impl Nav {
    pub(crate) fn place(&self) -> Place {
        Place::of(self.place.get().as_deref())
    }

    /// Show `place`, as a capture stages it.
    #[cfg(feature = "samples")]
    pub(crate) fn show(&self, place: Place) {
        self.show_place(place);
    }

    /// Show `place`: what a link or the person's own card asks for.
    pub(crate) fn show_place(&self, place: Place) {
        self.place.set(Some(place.key().to_owned()));
        self.pane.set(ui::widget::NavPane::Content);
    }

    /// Show the session `id`, no action of it open.
    pub(crate) fn open_session(&self, id: &str) {
        self.action.set(None);
        self.full_output.set(None);
        self.session.set(Some(id.to_owned()));
    }

    /// Show the session `id` from wherever the window is: a session's page is the Agents place, so
    /// opening the session a link names shows it there.
    pub(crate) fn show_session(&self, id: &str) {
        self.place.set(Some(Place::Agents.key().to_owned()));
        self.pane.set(ui::widget::NavPane::Content);
        self.open_session(id);
    }

    /// Show the card of the agent `client`, and carry out `command` there.
    pub(crate) fn show_agent(&self, client: &str, command: Option<effects::AgentCommand>) {
        self.place.set(Some(Place::Agents.key().to_owned()));
        self.pane.set(ui::widget::NavPane::Content);
        self.close_session();
        self.command.set(command);
        self.agent.set(Some(client.to_owned()));
    }

    /// Go back one step: from a session to the card it was opened from or every session, from a
    /// card to every session.
    pub(crate) fn back(&self) {
        if self.session.get().is_some() {
            self.close_session();
        } else {
            self.agent.set(None);
        }
    }

    /// Show every session again.
    pub(crate) fn close_session(&self) {
        self.action.set(None);
        self.full_output.set(None);
        self.session.set(None);
    }
}

/// What every place reads, followed once for the window: a place shown again, or a session
/// chosen, reads the answers already here rather than asking anew.
#[derive(Clone)]
pub(crate) struct Reads {
    pub(crate) sessions: Query<wire::Sessions>,
    pub(crate) server: Query<mcp_server::Server>,
}

impl Reads {
    /// Every session the history listed, once it answered: none before, so a session's address
    /// resolves meanwhile without its session.
    pub(crate) fn sessions(&self) -> Vec<wire::AgentSession> {
        self.sessions.answer().with(|read| match read {
            Arrival::Ready(sessions) => sessions.sessions.clone(),
            Arrival::Pending { .. } | Arrival::Failed(_) => Vec::new(),
        })
    }
}

/// A server the app reads and asks through: the workspaces' history or the MCP server.
#[derive(Clone)]
pub(crate) struct Resources(ResourceEnv<Console>);

impl Resources {
    pub(crate) fn of(cx: &ViewCx<Console>) -> Self {
        Self(ResourceEnv::clone(cx))
    }

    fn ask(&self, target: RemoteActionTarget, argument: InvokeArgument) {
        self.0.invoke(target, argument);
    }

    /// Ask the history for `action` with `value` as its argument.
    pub(crate) fn history(&self, action: &str, value: &impl serde::Serialize) {
        self.ask(wire::target(action), wire::argument(value));
    }

    /// Ask the MCP server for `action` with `value` as its argument.
    pub(crate) fn server(&self, action: &str, value: &impl serde::Serialize) {
        self.ask(mcp_server::target(action), mcp_server::argument(value));
    }

    /// Take the key `device` (hex) back, as every list of the person's keys does: their root
    /// revokes it, and the record of their keys marks it, which ends its sessions everywhere.
    pub(crate) fn revoke_key(&self, device: &str) {
        use ui::tilo_actions::remote::device_tickets;
        self.ask(
            device_tickets::target(device_tickets::REVOKE),
            device_tickets::Revoke::retired(device).argument(),
        );
    }

    /// Show the person how their agent connects as them: the setup their own card shows, with
    /// the one line that sets Tilo up on the agent's computer.
    pub(crate) fn show_connect(&self) {
        use ui::tilo_actions::remote::session_actions;
        self.ask(
            session_actions::target(session_actions::SHOW_CONNECT),
            InvokeArgument::Json(serde_json::Map::new()),
        );
    }

    /// Follow the history's read `action` with `value` as its argument.
    pub(crate) fn follow_history<T>(&self, action: &str, value: &impl serde::Serialize) -> Query<T>
    where
        T: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        self.0.query(wire::target(action), wire::argument(value))
    }

    /// Follow the MCP server.
    pub(crate) fn follow_server(&self) -> Query<mcp_server::Server> {
        self.0
            .query(mcp_server::target(mcp_server::SERVER), InvokeArgument::None)
    }

    /// Ask for the link to `resource`, as the framework's `share-link` hands it out:
    /// copying it is the framework's, so a link to a session is the same link any resource
    /// gets.
    pub(crate) fn share_link(&self, resource: &str) {
        self.0.dispatcher().share_link(resource);
    }

    /// Open the full desktop where `href`, a `tilo://open#…` address, says.
    pub(crate) fn open_in_tilo(&self, href: &str) {
        // The address is the MCP server's, and an open the session refuses is told where the open
        // lands; one that does not parse names nothing to open.
        let _ = self.0.dispatcher().open_link(href);
    }
}

pub(crate) fn tr(key: &str) -> String {
    crate::locales::CATALOG.t(key)
}

pub(crate) fn trf(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::locales::CATALOG.t_args(key, args)
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

/// A row led by `author`'s face: a person's round avatar, or an agent's square one with the
/// agent mark, so the two are never mistaken — both in the colour presence paints them in.
pub(crate) fn faced(row: ListRow, author: &wire::Author) -> ListRow {
    ui::author_face(row, author)
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
    const TITLE_KEY: &'static str = "console-app-title";
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
            session: cx.local(None),
            action: cx.local(None),
            full_output: cx.local(None),
            agent: cx.local(None),
            command: cx.local(None),
        };
        #[cfg(feature = "samples")]
        {
            let stage = nav.clone();
            cx.on_stage(move |scenario| sample::stage(scenario, &stage));
        }
        let records = Resources::of(cx);
        let reads = Reads {
            sessions: records.follow_history(wire::SESSIONS, &agents::query()),
            server: records.follow_server(),
        };
        let opened = nav.clone();
        cx.effects(move |effect: ConsoleEffect, _cx| {
            match effects::agent(&effect) {
                Some((client, command)) => opened.show_agent(client, command),
                None => match &effect {
                    ConsoleEffect::ViewSession { session } => opened.show_session(session),
                    ConsoleEffect::ViewConnect => opened.show_place(Place::Connect),
                    _ => {}
                },
            }
            Ok(ui::framework::Effected::done())
        });
        let (listed, named) = (reads.clone(), reads.clone());
        let server_read = reads.server.clone();
        let filter = cx.value(Console::clone);
        let content = cx.switch(&filter, move |cx, _| {
            let (nav, reads) = (nav.clone(), reads.clone());
            let (place, session) = (nav.place.clone(), nav.session.clone());
            cx.switch(&place, move |cx, _| {
                let (nav, reads) = (nav.clone(), reads.clone());
                let (nav, reads, agent) = (nav.clone(), reads, nav.agent);
                cx.switch(&session, move |cx, _| {
                    let (nav, reads) = (nav.clone(), reads.clone());
                    cx.switch(&agent, move |cx, _| window(cx, &nav, &reads))
                })
            })
        });
        cx.publishing(
            Publishes::new()
                .live_rows(move || {
                    let sessions = listed.sessions();
                    let server = server_read.answer().with(|read| match read {
                        Arrival::Ready(server) => server.clone(),
                        Arrival::Pending { .. } | Arrival::Failed(_) => {
                            mcp_server::Server::default()
                        }
                    });
                    let mut rows = agents::live_sessions(&sessions);
                    let agent_names = sessions
                        .iter()
                        .map(|session| session.agent.name.as_str())
                        .chain(server.grants.iter().map(|grant| grant.agent.as_str()));
                    rows.extend(
                        agent_card::clients(agent_names)
                            .iter()
                            .map(|client| agent_card::live_agent(client)),
                    );
                    rows
                })
                .live_row(move |resource| {
                    agents::live_session_at(&named.sessions(), resource)
                        .or_else(|| agent_card::agent_of(resource).map(agent_card::live_agent))
                }),
            content,
        )
    }
}

/// The window as it shows `nav`'s place: the sidebar of places, and the place with its
/// commands in the header.
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
    .on_choose("console.places", move |_| {
        chosen.pane.set(ui::widget::NavPane::Content);
    });

    let place = nav.place();
    let (open, close) = (nav.clone(), nav.clone());
    let shell = ui::widget::NavShell::new()
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("console.shell")
        .mode(ui::widget::NavMode::Push)
        .with_window_toolbar()
        .with_pane_state(&nav.pane)
        .sidebar(Box::new(sidebar))
        .sidebar_header(tr("console-title"), Vec::new())
        .on_back(
            move || open.session.get().is_some() || open.agent.get().is_some(),
            move || close.back(),
        );
    let (shell, location) = match place {
        Place::Agents if nav.session.get().is_none() && nav.agent.get().is_some() => {
            let client = nav.agent.get().unwrap_or_default();
            let shell = shell.content(agent_card::view(cx, nav, reads, &client));
            (shell, client)
        }
        Place::Agents => (
            shell.content(agents::view(cx, nav, reads)),
            tr(place.label()),
        ),
        Place::Workspaces => (
            shell.content(workspaces::view(cx, reads)),
            tr(place.label()),
        ),
        Place::Connect => (shell.content(connect::view(cx, reads)), tr(place.label())),
        Place::Usage => (shell.content(usage::view()), tr(place.label())),
    };
    let link = shell.window_bar_link();
    Box::new(window::ConsoleWindow::new(
        Box::new(shell),
        cx.toolbar(),
        link,
        location,
    ))
}

ui::dev_reload_entry!(App);

#[cfg(test)]
tilo_app_test_host::reach_tests!(App);
