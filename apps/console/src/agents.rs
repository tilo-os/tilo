//! Agents: every session of every agent across the person's workspaces, and the session chosen, as
//! the timeline of its actions.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::mcp_server;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    ButtonRole, Fact, Facts, ListRow, ObjectHeader, Section, SectionHeader, Surface, SurfaceLevel,
    Tone,
};

use crate::effects::ConsoleEffect;
use crate::{Console, Nav, Reads, Resources, ago, faced, page, tr, trf};

/// What Agents reads: every session in every workspace.
pub(crate) fn query() -> wire::SessionsQuery {
    wire::SessionsQuery {
        scope: wire::Scope::AllWorkspaces,
        limit: 0,
    }
}

pub(crate) const SESSION_KIND: &str = "console.session";

pub(crate) use console_model::Session;

/// How many of the sessions read are published as resources of their own: a published tree
/// stays bounded, and any other session's address is answered by [`live_session`] alone.
pub(crate) const SESSION_RESOURCE_LIMIT: usize = 16;

/// The address of the session `id`: what a link to it names.
pub(crate) fn session_resource_id(id: &str) -> String {
    format!("{SESSION_KIND}:{id}")
}

/// The session whose address `resource` is, when it is a session's.
pub(crate) fn session_id(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(SESSION_KIND)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

/// The view a session opens in: its page in this window, opened by [`ConsoleEffect::ViewSession`].
fn session_view() -> ui::tilo_actions::ResourceViewSpec {
    ui::tilo_actions::ResourceViewSpec::new("session", "Session", ConsoleEffect::VIEW_SESSION.id)
        .with_primary(true)
        .on_desktop()
}

/// The session `id` as the resource its address names, labelled by the session when `sessions` lists
/// it: its page in this window, and its link.
pub(crate) fn live_session(sessions: &[wire::AgentSession], id: &str) -> LiveRow {
    let label = sessions
        .iter()
        .find(|session| session.id == id)
        .map_or_else(
            || tr("console-session"),
            |session| session.instruction.clone(),
        );
    LiveRow::typed_with::<Session>(
        session_resource_id(id),
        label,
        &ui::tilo_resource::RowBindings::new().bind(
            ConsoleEffect::VIEW_SESSION,
            [("session", serde_json::Value::from(id.to_owned()))],
        ),
    )
    .view(session_view())
}

/// The sessions `sessions` lists as resources of their own, up to the limit, first session first.
pub(crate) fn live_sessions(sessions: &[wire::AgentSession]) -> Vec<LiveRow> {
    sessions
        .iter()
        .take(SESSION_RESOURCE_LIMIT)
        .map(|session| live_session(sessions, &session.id))
        .collect()
}

/// The row the address `resource` names, when it names a session: a link opens it before the
/// history has answered, so the session's page shows at once and the session arrives on it.
pub(crate) fn live_session_at(sessions: &[wire::AgentSession], resource: &str) -> Option<LiveRow> {
    Some(live_session(sessions, session_id(resource)?))
}

/// The sessions, beside the MCP server that says which agents are connected.
#[derive(Clone, Default)]
pub(crate) struct Seen {
    pub(crate) history: wire::Sessions,
    pub(crate) server: mcp_server::Server,
}

impl Seen {
    /// The MCP server's open agent session `session` is, with the grant it came in by: History names a
    /// session as the MCP server does ("Claude Code #2").
    pub(crate) fn open(&self, session: &wire::AgentSession) -> Option<mcp_server::AgentSessionRef> {
        self.server
            .session(&session.agent.name)
            .map(|(grant, session)| mcp_server::AgentSessionRef {
                grant: grant.id.clone(),
                session: session.name.clone(),
            })
    }

    /// Whether `session` is working now: its agent session through the MCP server is open and it is that
    /// session's newest work.
    pub(crate) fn working(&self, session: &wire::AgentSession) -> bool {
        self.open(session).is_some()
            && self
                .history
                .sessions
                .iter()
                .filter(|other| other.agent.name == session.agent.name)
                .max_by_key(|other| other.last_ms)
                .is_some_and(|newest| newest.id == session.id)
    }

    /// The workspace `session` worked in, as the MCP server lists it.
    pub(crate) fn workspace(&self, session: &wire::AgentSession) -> Option<&mcp_server::Workspace> {
        let name = session.workspace.as_deref()?;
        self.server
            .workspaces
            .iter()
            .find(|workspace| workspace.name == name || workspace.id == name)
    }
}

/// The place's content: every session, or the session chosen, whose way back is the shell's.
pub(crate) fn view(cx: &ViewCx<Console>, nav: &Nav, reads: &Reads) -> Element {
    let sessions = &reads.sessions;
    let server_state = reads.server.answer().state();
    let chosen = nav.session.get();
    let nav = nav.clone();
    Box::new(cx.arriving(&sessions.answer(), move |cx, sessions| {
        let (nav, sessions, chosen) = (nav.clone(), sessions.clone(), chosen.clone());
        cx.switch(&server_state, move |cx, server| {
            let seen = Seen {
                history: sessions.clone(),
                server: ready(server),
            };
            let session = chosen.as_deref().and_then(|id| {
                seen.history
                    .sessions
                    .iter()
                    .find(|session| session.id == id)
            });
            match (session, chosen.as_deref()) {
                (Some(session), _) => inspect(cx, &nav, &seen, session),
                (None, Some(_)) => unknown(&seen),
                (None, None) => list(cx, &nav, &seen),
            }
        })
    })) as Element
}

/// The MCP server as far as it answered: nothing connected until it does, and nothing where no
/// server is served.
pub(crate) fn ready<E>(server: &Arrival<mcp_server::Server, E>) -> mcp_server::Server {
    match server {
        Arrival::Ready(server) => server.clone(),
        Arrival::Pending { .. } | Arrival::Failed(_) => mcp_server::Server::default(),
    }
}

/// A session's page whose session the history has not listed: still being read, or not here at all
/// (a workspace this person is not in, a session long gone).
pub(crate) fn unknown(seen: &Seen) -> Element {
    let state = if seen.history.pending {
        EmptyState::new(tr("console-session-pending")).with_icon(Icon::Clock)
    } else {
        EmptyState::new(tr("console-session-missing"))
            .with_message(tr("console-session-missing-message"))
            .with_icon(Icon::Bot)
    };
    page(vec![Box::new(state.with_id("console-session-missing"))])
}

/// Every session the resource lets show: working ones first, then the rest.
fn list(cx: &ViewCx<Console>, nav: &Nav, seen: &Seen) -> Element {
    let filter = cx.value(Console::clone).get();
    let mut parts = vec![filters(cx, seen, &filter)];
    let shown: Vec<&wire::AgentSession> = seen
        .history
        .sessions
        .iter()
        .filter(|session| crate::console::shows(&filter, session))
        .collect();
    if shown.is_empty() {
        let state = if seen.history.pending {
            EmptyState::new(tr("console-sessions-pending")).with_icon(Icon::Clock)
        } else if filter.narrows() {
            EmptyState::new(tr("console-sessions-none-here"))
                .with_message(tr("console-sessions-none-here-message"))
                .with_icon(Icon::Bot)
        } else {
            EmptyState::new(tr("console-sessions-empty"))
                .with_message(tr("console-sessions-empty-message"))
                .with_icon(Icon::Bot)
        };
        parts.push(Box::new(state.with_id("console-sessions-empty")));
        return page(parts);
    }
    let (working, earlier): (Vec<&wire::AgentSession>, Vec<&wire::AgentSession>) =
        shown.into_iter().partition(|session| seen.working(session));
    let chosen = nav.session.get();
    for (heading, sessions) in [
        ("console-sessions-working", working),
        ("console-sessions-earlier", earlier),
    ] {
        if sessions.is_empty() {
            continue;
        }
        let rows = sessions
            .into_iter()
            .map(|session| {
                session_row(
                    nav,
                    seen,
                    session,
                    chosen.as_deref() == Some(session.id.as_str()),
                )
            })
            .collect();
        parts.push(Box::new(
            Section::new(rows).with_header(SectionHeader::group(tr(heading))),
        ));
    }
    page(parts)
}

pub(crate) fn session_row(
    nav: &Nav,
    seen: &Seen,
    session: &wire::AgentSession,
    selected: bool,
) -> Element {
    let working = seen.working(session);
    let mut row = faced(
        ListRow::new(
            format!("console-session-{}", session.id),
            session.instruction.clone(),
        ),
        &session.agent,
    )
    .subtitle(
        session
            .now
            .as_ref()
            .filter(|_| working)
            .map_or_else(|| started_by(&session.agent), now_line),
    )
    .secondary(
        session
            .workspace
            .clone()
            .unwrap_or_else(|| session_size(session)),
    )
    .meta(ago(session.last_ms))
    .selected(selected);
    if working {
        row = row.active();
    }
    let (chosen, id) = (nav.clone(), session.id.clone());
    Box::new(row.on_open(move || chosen.open_session(&id)))
}

/// Who started a session, as its row says it: "Started by you" for the reader's own, the
/// starter's name for someone else's.
fn started_by(agent: &wire::Author) -> String {
    starter(agent).unwrap_or_else(|| agent.name.clone())
}

/// The line naming who started a session, when the agent worked for someone.
fn starter(agent: &wire::Author) -> Option<String> {
    if agent.for_reader {
        return Some(tr("console-session-started-by-you"));
    }
    agent
        .on_behalf_of
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(|name| {
            trf(
                "console-session-started-by",
                &[("name", name.to_owned().into())],
            )
        })
}

/// How much a session did, as its row says it: the changes it made, as its finished notice
/// counts them ("4 changes" for four paragraphs written in one note), else the commands and
/// pages it went through.
fn session_size(session: &wire::AgentSession) -> String {
    match session.changes() {
        0 => trf(
            "console-session-actions",
            &[("count", session.actions.len().into())],
        ),
        changes => trf("console-session-changes", &[("count", changes.into())]),
    }
}

/// What a working session is doing now, as its row says it: "Running `cargo test`…", "Editing
/// src/email.rs".
pub(crate) fn now_line(now: &wire::Now) -> String {
    match now {
        wire::Now::Running { command, .. } => trf(
            "console-now-running",
            &[("command", command.clone().into())],
        ),
        wire::Now::Editing { path, .. } => {
            trf("console-now-editing", &[("path", path.clone().into())])
        }
        wire::Now::Browsing { url, title, .. } => trf(
            "console-now-browsing",
            &[(
                "page",
                if title.is_empty() { url } else { title }.clone().into(),
            )],
        ),
        wire::Now::Changing { summary, .. } => summary.clone(),
        wire::Now::Ran { command, .. } => {
            trf("console-now-ran", &[("command", command.clone().into())])
        }
    }
}

/// A dropdown of `options` (value, label) bound to `field` of the resource, each option under
/// the heading its group carries in the open list.
fn picker(
    cx: &ViewCx<Console>,
    id: &'static str,
    name: &str,
    options: Vec<(String, Vec<(String, String)>)>,
    field: Path<Console, String>,
) -> Element {
    let name = tr(name);
    Box::new(cx.control(
        move |state| {
            let mut picker = Dropdown::from_state(Vec::new(), state)
                .with_id(id)
                .with_accessible_name(name);
            picker.set_grouped_value_options(options);
            picker
        },
        field,
    ))
}

/// The agent picker's options: every agent, then each client with all of its sessions under it,
/// so a person narrows to one session or to the client behind every one of them.
fn agent_options(seen: &Seen) -> Vec<(String, Vec<(String, String)>)> {
    let mut groups: Vec<(String, Vec<(String, String)>)> = vec![(
        String::new(),
        vec![(String::new(), tr("console-every-agent"))],
    )];
    for session in &seen.history.sessions {
        let client = crate::client::of(&session.agent.name);
        let known = groups
            .iter()
            .skip(1)
            .position(|(heading, _)| heading == client.name());
        let at = known.map_or_else(
            || {
                groups.push((
                    client.name().to_owned(),
                    vec![(
                        client.name().to_owned(),
                        trf(
                            "console-client-every-session",
                            &[("client", client.name().to_owned().into())],
                        ),
                    )],
                ));
                groups.len().saturating_sub(1)
            },
            |found| found.saturating_add(1),
        );
        if let Some((_, sessions)) = groups.get_mut(at)
            && !sessions
                .iter()
                .any(|(value, _)| value == &session.agent.name)
        {
            sessions.push((session.agent.name.clone(), session.agent.name.clone()));
        }
    }
    groups
}

/// The workspace picker's options: every workspace, with no heading over them, and then
/// each workspace by name.
fn workspace_options(seen: &Seen) -> Vec<(String, Vec<(String, String)>)> {
    let mut workspaces: Vec<(String, String)> =
        vec![(String::new(), tr("console-every-workspace"))];
    for session in &seen.history.sessions {
        if let Some(workspace) = &session.workspace
            && !workspaces.iter().any(|(value, _)| value == workspace)
        {
            workspaces.push((workspace.clone(), workspace.clone()));
        }
    }
    vec![(String::new(), workspaces)]
}

/// The row narrowing the sessions by agent and workspace, with a way back to all of them while
/// it narrows them, and only then.
fn filters(cx: &ViewCx<Console>, seen: &Seen, filter: &Console) -> Element {
    let paths = Console::paths();
    let mut row = Row::new()
        .with_rhythm(Rhythm::Related)
        .push(picker(
            cx,
            "console-filter-agent",
            "console-filter-agent",
            agent_options(seen),
            paths.agent,
        ))
        .push(picker(
            cx,
            "console-filter-workspace",
            "console-filter-workspace",
            workspace_options(seen),
            paths.workspace,
        ));
    if filter.narrows() {
        row = row.push(Box::new(cx.actions().bind(
            Button::new("console-show-everything", "console-show-everything").with_icon(Icon::X),
            Console::SHOW_EVERYTHING,
        )));
    }
    Box::new(row)
}

/// The chosen session: what it was asked, what can be done with it, and its actions.
pub(crate) fn inspect(
    cx: &ViewCx<Console>,
    nav: &Nav,
    seen: &Seen,
    session: &wire::AgentSession,
) -> Element {
    let records = Resources::of(cx);
    let mut facts = vec![Fact::with_icon(Icon::Bot, session.agent.name.clone())];
    if let Some(starter) = starter(&session.agent) {
        facts.push(Fact::with_icon(Icon::User, starter));
    }
    facts.extend(
        session
            .workspace
            .clone()
            .map(|workspace| Fact::with_icon(Icon::LayoutGrid, workspace)),
    );
    facts.push(Fact::with_icon(Icon::Clock, ago(session.started_ms)));
    let mut parts: Vec<Element> = vec![
        Box::new(ObjectHeader::new(session.instruction.clone())),
        Box::new(Facts::new(facts)),
    ];
    parts.push(commands(&records, seen, session));
    if let Some(blocked) = &session.undo_blocked {
        parts.push(Box::new(
            Text::new(blocked)
                .with_role(TextRole::Caption)
                .dimmed()
                .with_id("console-session-undo-blocked"),
        ));
    }
    if let Some(decision) = &session.decision {
        parts.push(Box::new(Section::titled(
            tr("console-session-why"),
            vec![Box::new(Text::new(decision))],
        )));
    }
    let nav = nav.clone();
    let shown = session.clone();
    parts.push(cx.switch(&nav.action.clone(), move |cx, open| {
        let (nav, shown, open) = (nav.clone(), shown.clone(), *open);
        cx.switch(&nav.full_output.clone(), move |cx, full| {
            let Some((index, session)) =
                full.and_then(|index| Some((index, more_of(&shown, index)?)))
            else {
                return actions(cx, &nav, &shown, open, None);
            };
            let read = Resources::of(cx)
                .follow_history::<wire::SessionOutput>(wire::SESSION_OUTPUT, &session);
            let (nav, shown) = (nav.clone(), shown.clone());
            Box::new(cx.arriving(&read.answer(), move |cx, whole| {
                actions(cx, &nav, &shown, open, Some((index, whole)))
            }))
        })
    }));
    page(parts)
}

/// Stop, Take over and Undo, as the session allows them.
pub(crate) fn commands(records: &Resources, seen: &Seen, session: &wire::AgentSession) -> Element {
    let mut row = Row::new().with_rhythm(Rhythm::Related);
    if seen.open(session).is_some() && seen.working(session) {
        let (records, seen, session) = (records.clone(), seen.clone(), session.clone());
        row = row.push(Box::new(
            Button::new("console-session-stop", "console-session-stop")
                .with_icon(Icon::Square)
                .role(ButtonRole::Destructive)
                .on_open(move || {
                    stop(&records, &seen, &session);
                }),
        ));
    }
    if take_over_target(seen, session).is_some() {
        let (records, seen, session) = (records.clone(), seen.clone(), session.clone());
        row = row.push(Box::new(
            Button::new("console-session-take-over", "console-session-take-over")
                .with_icon(Icon::Hand)
                .on_open(move || take_over(&records, &seen, &session)),
        ));
    }
    let (undo, undone) = (records.clone(), session.clone());
    row = row.push(Box::new(
        Button::new("console-session-undo", "console-session-undo")
            .with_icon(Icon::Undo2)
            .with_enabled(session.undo_blocked.is_none())
            .on_open(move || {
                undo_session(&undo, &undone);
            }),
    ));
    let link = records.clone();
    let resource = session_resource_id(&session.id);
    row = row.push(Box::new(
        Button::new("console-session-copy-link", "console-session-copy-link")
            .with_icon(Icon::Link)
            .on_open(move || link.share_link(&resource)),
    ));
    Box::new(row)
}

/// Where taking `session` over opens: the resource of its last action, else its workspace in the
/// full desktop.
fn take_over_target(
    seen: &Seen,
    session: &wire::AgentSession,
) -> Option<Result<wire::Target, String>> {
    session
        .actions
        .last()
        .map(|action| Ok(action.view.target().clone()))
        .or_else(|| {
            seen.workspace(session)
                .map(|workspace| Err(workspace.open.clone()))
        })
}

/// End `session` now, keeping its grant, when it is working.
pub(crate) fn stop(records: &Resources, seen: &Seen, session: &wire::AgentSession) -> bool {
    match seen.open(session).filter(|_| seen.working(session)) {
        Some(open) => {
            records.server(mcp_server::STOP, &open);
            true
        }
        None => false,
    }
}

/// Take `session` over: stop the agent and open where it was working.
pub(crate) fn take_over(records: &Resources, seen: &Seen, session: &wire::AgentSession) {
    stop(records, seen, session);
    match take_over_target(seen, session) {
        Some(Ok(target)) => records.history(wire::OPEN, &target),
        Some(Err(href)) => records.open_in_tilo(&href),
        None => {}
    }
}

/// Undo all of `session`, when the history lets it.
pub(crate) fn undo_session(records: &Resources, session: &wire::AgentSession) -> bool {
    if session.undo_blocked.is_some() {
        return false;
    }
    records.history(wire::UNDO_SESSION, &session.range);
    true
}

const fn kind_icon(view: &wire::ActionView) -> Icon {
    match view {
        wire::ActionView::File { .. } => Icon::FileText,
        wire::ActionView::Command { .. } => Icon::Terminal,
        wire::ActionView::Page { .. } => Icon::Globe,
        wire::ActionView::Record { .. } => Icon::AppWindow,
    }
}

/// The session's actions, oldest first, the one `open` names with its view under it.
fn actions(
    cx: &ViewCx<Console>,
    nav: &Nav,
    session: &wire::AgentSession,
    open: Option<usize>,
    full: Option<(usize, &wire::SessionOutput)>,
) -> Element {
    let records = Resources::of(cx);
    let mut rows: Vec<Element> = Vec::new();
    for (index, action) in session.actions.iter().enumerate() {
        let target = action.view.target();
        let mut row = ListRow::new(format!("console-action-{index}"), action.summary.clone())
            .icon(kind_icon(&action.view))
            .secondary(target.label.clone())
            .meta(ago(action.at_ms))
            .selected(open == Some(index));
        if let Some(offer) = &action.undo {
            let records = records.clone();
            let query = wire::ResourceQuery {
                range: session.range.clone(),
                task: None,
                app_id: action.app_id.clone(),
                id: action.record.clone(),
            };
            let (kind, range) = (offer.action, action.range.clone());
            row = row.trailing(Box::new(
                Button::new(&format!("console-action-{index}-undo"), &offer.label)
                    .role(ButtonRole::Plain)
                    .on_open(move || match kind {
                        wire::DetailAction::UndoEntry => {
                            records.history(wire::UNDO_RECORD, &query);
                        }
                        _ => records.history(wire::UNDO, &range),
                    }),
            ));
        }
        let chosen = nav.clone();
        row = row.on_open(move || {
            chosen
                .action
                .update(|open| *open = (*open != Some(index)).then_some(index));
        });
        rows.push(Box::new(row));
        if open == Some(index) {
            rows.push(action_view(
                cx,
                nav,
                &records,
                index,
                action,
                full.filter(|(shown, _)| *shown == index)
                    .map(|(_, whole)| whole),
            ));
        }
    }
    Box::new(Section::titled(
        trf(
            "console-session-timeline",
            &[("count", session.actions.len().into())],
        ),
        rows,
    ))
}

/// What one action shows when opened: never just its sentence.
fn action_view(
    cx: &ViewCx<Console>,
    nav: &Nav,
    records: &Resources,
    index: usize,
    action: &wire::SessionAction,
    whole: Option<&wire::SessionOutput>,
) -> Element {
    let id = format!("console-action-{index}-view");
    let target = action.view.target().clone();
    let open = {
        let (records, target) = (records.clone(), target.clone());
        Button::new(&format!("{id}-open"), "console-action-open")
            .with_icon(Icon::ExternalLink)
            .on_open(move || records.history(wire::OPEN, &target))
    };
    let mut parts: Vec<Element> = Vec::new();
    match &action.view {
        wire::ActionView::File { path, lines, .. } => {
            parts.push(code_line(path, None));
            if lines.is_empty() {
                parts.push(compared(cx, records, &action.range));
            } else {
                parts.push(well(lines.iter().map(diff_line).collect()));
            }
        }
        wire::ActionView::Command {
            command,
            cwd,
            output,
            more,
            exit,
            started_ms,
            ended_ms,
            execution,
            ..
        } => {
            if !cwd.is_empty() {
                parts.push(Box::new(
                    Text::new(&trf("console-command-in", &[("cwd", cwd.clone().into())]))
                        .with_role(TextRole::Caption)
                        .dimmed()
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
                ));
            }
            if execution.is_some() && *more {
                let output = Output {
                    index,
                    prompt: format!("$ {command}"),
                    tail: output.clone(),
                };
                parts.push(output_part(&nav.full_output, whole, &output));
            } else {
                let mut lines = vec![code_line(&format!("$ {command}"), None)];
                lines.extend(output.iter().map(|line| code_line(line, None)));
                parts.push(well(lines));
            }
            parts.push(command_status(
                action.view.is_running(),
                *exit,
                *started_ms,
                *ended_ms,
            ));
        }
        wire::ActionView::Page { url, title, .. } => {
            let title = if title.is_empty() {
                &target.label
            } else {
                title
            };
            parts.push(Box::new(
                Text::new(title).with_role(TextRole::Label).strong(),
            ));
            parts.push(code_line(url, None));
        }
        wire::ActionView::Record { .. } => {
            let mut place = vec![Fact::with_icon(Icon::AppWindow, target.label.clone())];
            place.extend(target.app_label.map(Fact::text));
            parts.push(Box::new(Facts::new(place)));
            parts.push(compared(cx, records, &action.range));
        }
    }
    parts.push(Box::new(
        Row::new().with_rhythm(Rhythm::Related).push(Box::new(open)),
    ));
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related);
    for part in parts {
        column = column.push(part);
    }
    Box::new(
        Container::new()
            .with_inset(Rhythm::Related)
            .with_id(&id)
            .with_child(Box::new(column)),
    )
}

/// How a command ended, as a caption: still running, or its exit status and how long it
/// took, a failure in the danger ink.
fn command_status(
    running: bool,
    exit: Option<i32>,
    started_ms: Option<i64>,
    ended_ms: Option<i64>,
) -> Element {
    if running {
        return Box::new(
            Text::new(&tr("console-command-running"))
                .with_role(TextRole::Caption)
                .tone(Tone::Info)
                .with_id("console-command-running"),
        );
    }
    let Some(code) = exit else {
        return Box::new(Column::new().with_no_padding().with_intrinsic_height());
    };
    let took = started_ms
        .zip(ended_ms)
        .and_then(|(started, ended)| u64::try_from(ended.saturating_sub(started)).ok())
        .map(|ms| ui::format::duration(std::time::Duration::from_millis(ms)));
    let said = match took {
        Some(took) => trf(
            "console-command-exit-after",
            &[("code", code.into()), ("took", took.into())],
        ),
        None => trf("console-command-exit", &[("code", code.into())]),
    };
    let said = Text::new(&said).with_role(TextRole::Caption);
    Box::new(if code == 0 {
        said.dimmed()
    } else {
        said.tone(Tone::Danger)
    })
}

/// One command's output as its view shows it: the command's place in the session, its prompt
/// line and the last lines it printed.
struct Output {
    index: usize,
    prompt: String,
    tail: Vec<String>,
}

/// A command's output: its last lines, or every line once the person asks, read from the
/// session's resource as the history serves it.
fn output_part(
    shown: &LocalState<Option<usize>>,
    whole: Option<&wire::SessionOutput>,
    output: &Output,
) -> Element {
    let index = output.index;
    let full = whole.is_some();
    let toggle = {
        let shown = shown.clone();
        Button::new(
            &format!("console-action-{index}-output"),
            if full {
                "console-command-less"
            } else {
                "console-command-full"
            },
        )
        .role(ButtonRole::Plain)
        .on_open(move || shown.update(|open| *open = (!full).then_some(index)))
    };
    let printed: Element = match whole {
        Some(whole) if whole.unavailable => {
            Box::new(Text::new(&tr("console-command-output-unavailable")).dimmed())
        }
        Some(whole) => well(
            std::iter::once(&output.prompt)
                .chain(&whole.lines)
                .map(|line| code_line(line, None))
                .collect(),
        ),
        None => well(
            std::iter::once(&output.prompt)
                .chain(&output.tail)
                .map(|line| code_line(line, None))
                .collect(),
        ),
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(printed)
            .push(Box::new(toggle)),
    )
}

/// The session resource of the command at `index` of `session`, when it printed more than its view
/// shows.
fn more_of(session: &wire::AgentSession, index: usize) -> Option<wire::CommandExecution> {
    match &session.actions.get(index)?.view {
        wire::ActionView::Command {
            execution: Some(record),
            more: true,
            ..
        } => Some(record.clone()),
        _ => None,
    }
}

/// What the action's changes did, as the history compares the workspace either side of
/// them: each field before and after, or else each change in a line.
fn compared(cx: &ViewCx<Console>, records: &Resources, range: &wire::Range) -> Element {
    let read = records.follow_history::<wire::Detail>(
        wire::DETAIL,
        &wire::DetailQuery {
            range: range.clone(),
            compare: true,
        },
    );
    Box::new(cx.arriving(&read.answer(), move |_, detail| {
        if !detail.changes.is_empty() {
            let mut lines = Vec::new();
            for change in &detail.changes {
                lines.push(code_line(&change.field, Some(TextRole::Label)));
                if !change.before.is_empty() {
                    lines.push(diff_line(&wire::DiffLine {
                        side: wire::DiffSide::Removed,
                        text: change.before.clone(),
                    }));
                }
                if !change.after.is_empty() {
                    lines.push(diff_line(&wire::DiffLine {
                        side: wire::DiffSide::Added,
                        text: change.after.clone(),
                    }));
                }
            }
            return well(lines);
        }
        match &detail.comparison {
            Some(wire::Comparison::Changes { lines }) if !lines.is_empty() => {
                well(lines.iter().map(|line| code_line(line, None)).collect())
            }
            _ => Box::new(Text::new(&tr("console-action-no-preview")).dimmed()),
        }
    }))
}

/// A line of code or output, in the code role unless `role` says otherwise.
fn code_line(text: &str, role: Option<TextRole>) -> Element {
    let text = if text.is_empty() { "\u{a0}" } else { text };
    Box::new(
        Text::new(text)
            .with_role(role.unwrap_or(TextRole::Code))
            .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
    )
}

/// A line of a file's change: what it removed in the danger ink, what it added in the
/// success ink, the rest as it reads.
fn diff_line(line: &wire::DiffLine) -> Element {
    let (mark, tone) = match line.side {
        wire::DiffSide::Both => ("  ", None),
        wire::DiffSide::Removed => ("− ", Some(Tone::Danger)),
        wire::DiffSide::Added => ("+ ", Some(Tone::Success)),
    };
    let text = Text::new(&format!("{mark}{}", line.text))
        .with_role(TextRole::Code)
        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph);
    Box::new(match tone {
        Some(tone) => text.tone(tone),
        None => text.dimmed(),
    })
}

/// `lines` in a raised well, as code and output read everywhere else.
fn well(lines: Vec<Element>) -> Element {
    let column = lines.into_iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Tight),
        Column::push,
    );
    Box::new(Surface::new(SurfaceLevel::Raised, Box::new(column)).padded(Rhythm::Tight))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_by(for_reader: bool, on_behalf_of: Option<&str>) -> wire::Author {
        wire::Author {
            name: "Claude Code".to_owned(),
            relation: wire::Relation::Software,
            for_reader,
            on_behalf_of: on_behalf_of.map(str::to_owned),
            ..wire::Author::default()
        }
    }

    /// A session's row names who started it, not the agent its face already shows.
    #[test]
    fn a_run_row_says_who_started_it() {
        assert_eq!(started_by(&session_by(true, Some("Ada"))), "Started by you");
        assert_eq!(
            started_by(&session_by(false, Some("Ada"))),
            "Started by Ada"
        );
        assert_eq!(started_by(&session_by(false, None)), "Claude Code");
    }
}
