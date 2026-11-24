app so.tilo.console "Console" (
    icon: .bot,
    category: .system,
    tagline: "What your agents do in your workspaces, and the MCP server they come in by",
    keywords: ["agents", "agent runs", "mcp", "connect an agent", "stop agent", "undo agent", "grants", "revoke", "agenten", "läufe", "verbinden"],
)

use so.tilo.history as history
use so.tilo.session as session
use so.tilo.device as device
use tilo.mcp-server as mcp

/// The window: which place it opens in. Everything else it shows is what the person narrowed
/// while it ran, which is the window's own and never kept.
type Window {
    /// The place the window opens in: `agents`, `workspaces`, `connect` or `usage`.
    place: Text = "agents"
}

root window: Window

default view console for Window

/// The window: the four places down one side, and the place shown beside them. Agents is the
/// person's own work seen through what their agents did: every session of every agent across the
/// workspaces this device holds, the session chosen as its timeline of actions, and one client's
/// card.
view console(w: Window) {
    state place: Text = w.place
    state session: Text = ""
    state agent: Text = ""
    state agent_filter: Text = ""
    state workspace_filter: Text = ""
    state opened: Text = ""
    state full: Text = ""
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    ui.NavShell(content_kind: .page, title: place_title(place), sidebar_title: "Console") {
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("agents", "Agents", icon: .bot) {
                    on select: set(place, "agents").then(set(session, "")).then(set(agent, ""))
                }
                ui.NavItem("workspaces", "Workspaces", icon: .layout_grid) {
                    on select: set(place, "workspaces").then(set(session, "")).then(set(agent, ""))
                }
                ui.NavItem("connect", "Connect", icon: .link) {
                    on select: set(place, "connect").then(set(session, "")).then(set(agent, ""))
                }
                ui.NavItem("usage", "Usage", icon: .gauge) {
                    on select: set(place, "usage").then(set(session, "")).then(set(agent, ""))
                }
            }
        }
        content: {
            if place == "agents" {
                agents(
                    sessions,
                    server,
                    session,
                    agent,
                    agent_filter,
                    workspace_filter,
                    opened,
                    full,
                )
            } else if place == "workspaces" {
                workspaces(server)
            } else if place == "connect" {
                connect(server)
            } else {
                usage()
            }
        }
    }
}

/// What the window is called in its bar.
fn place_title(place: Text) -> Text {
    if place == "agents" {
        std.say("Agents")
    } else if place == "workspaces" {
        std.say("Workspaces")
    } else if place == "connect" { std.say("Connect") } else { std.say("Usage") }
}

/// Agents: every session across the workspaces, narrowed by the agent and the workspace the
/// person chose, or the session chosen as its timeline, or one client's card.
view agents(
    sessions: history.Sessions,
    server: mcp.Server,
    session: State<Text>,
    agent: State<Text>,
    agent_filter: State<Text>,
    workspace_filter: State<Text>,
    opened: State<Text>,
    full: State<Text>,
) {
    if agent != "" {
        card(server, sessions, agent, session, agent, opened, full)
    } else if session != "" {
        if let s = sessions.sessions.find(one -> one.id == session) {
            session_page(s, sessions, server, session, agent, opened, full)
        } else {
            stack(gap: .section) {
                ui.Button("Back", icon: .arrow_left) {
                    on press: set(session, "")
                }
                ui.EmptyState(
                    if sessions.pending {
                        "Reading this session"
                    } else { "This session is not here" },
                    message: if sessions.pending {
                        ""
                    } else {
                        "The history lists no such session, or it worked in a workspace you are not in."
                    },
                    icon: if sessions.pending { .clock } else { .bot },
                )
            }
        }
    } else {
        let shown = sessions.sessions.filter(one -> shows(one, agent_filter, workspace_filter))
        let working_ones = shown.filter(one -> working(one, sessions.sessions, server))
        let earlier = shown.filter(one -> not working(one, sessions.sessions, server))
        stack(gap: .section) {
            filters(sessions, server, agent_filter, workspace_filter)
            if shown.is_empty() {
                if sessions.pending {
                    ui.EmptyState("Reading your workspaces' history", icon: .clock)
                } else if agent_filter != "" or workspace_filter != "" {
                    ui.EmptyState(
                        "No sessions match",
                        message: "No session of this agent in this workspace. Show everything to see the rest.",
                        icon: .bot,
                    )
                } else {
                    ui.EmptyState(
                        "No agent has worked here yet",
                        message: "Connect an agent and ask it for something: you can follow each of its sessions while it works.",
                        icon: .bot,
                    )
                }
            }
            if not working_ones.is_empty() {
                ui.Section("Working now") {
                    for s in working_ones {
                        session_row(s, sessions, server, session, opened)
                    }
                }
            }
            if not earlier.is_empty() {
                ui.Section("Earlier") {
                    for s in earlier {
                        session_row(s, sessions, server, session, opened)
                    }
                }
            }
            clients_section(server, sessions, agent, session)
        }
    }
}

/// The row narrowing the sessions by agent and workspace, with a way back to all of them while
/// it narrows them, and only then.
view filters(
    sessions: history.Sessions,
    server: mcp.Server,
    agent_filter: State<Text>,
    workspace_filter: State<Text>,
) {
    row(gap: .related, wrap: true, align: .center) {
        ui.Button("Every agent", toggled: agent_filter == "") {
            on press: set(agent_filter, "")
        }
        for client in clients_of(sessions, server) {
            ui.Button(client, toggled: agent_filter == client) {
                on press: set(agent_filter, if agent_filter == client { "" } else { client })
            }
        }
    }
    row(gap: .related, wrap: true, align: .center) {
        ui.Button("Every workspace", toggled: workspace_filter == "") {
            on press: set(workspace_filter, "")
        }
        for name in workspaces_of(sessions) {
            ui.Button(name, toggled: workspace_filter == name) {
                on press: set(workspace_filter, if workspace_filter == name { "" } else { name })
            }
        }
        if agent_filter != "" or workspace_filter != "" {
            ui.Button("Show everything", icon: .x) {
                on press: set(agent_filter, "").then(set(workspace_filter, ""))
            }
        }
    }
}

/// One session as its row says it: who did it and for whom, what it is doing now or how much it
/// did, in which workspace, and when.
view session_row(
    s: history.AgentSession,
    sessions: history.Sessions,
    server: mcp.Server,
    session: State<Text>,
    opened: State<Text>,
) {
    let is_working = working(s, sessions.sessions, server)
    ui.ListRow(
        title: s.instruction,
        note: if is_working and s.now != "" { now_line(s) } else { started_by(s.agent) },
        secondary: s.workspace ?? size_line(s),
        meta: when(s.last_ms),
        icon: if s.agent.relation == "software" { .bot } else { .file_text },
        selected: session == s.id,
    ) {
        on activate: set(session, s.id).then(set(opened, ""))
    }
}

/// The session chosen: what it was asked, what can be done with it, why it did what it did, and
/// its actions, each opening to what it did.
view session_page(
    s: history.AgentSession,
    sessions: history.Sessions,
    server: mcp.Server,
    session: State<Text>,
    agent: State<Text>,
    opened: State<Text>,
    full: State<Text>,
) {
    stack(gap: .section) {
        ui.Button("Back", icon: .arrow_left) {
            on press: set(session, "").then(set(opened, ""))
        }
        ui.ObjectHeader(title: s.instruction, caption: started_by(s.agent))
        commands(s, sessions, server)
        if let blocked = s.undo_blocked {
            ui.Text(blocked, role: .caption, secondary: true)
        }
        if let decision = s.decision {
            ui.Section("Why") {
                ui.Text(decision)
            }
        }
        timeline(s, opened, full)
    }
}

/// Stop, Take over and Undo, as the session allows them.
view commands(s: history.AgentSession, sessions: history.Sessions, server: mcp.Server) {
    row(gap: .related, wrap: true, align: .center) {
        if let grant = open_grant(s, server) {
            if working(s, sessions.sessions, server) {
                ui.Button("Stop", icon: .square, role: .destructive) {
                    on press: mcp.stop(
                        mcp.server,
                        session: mcp.AgentSessionRef(grant: grant.id, session: s.agent.name),
                    )
                }
            }
            ui.Button("Take over", icon: .hand) {
                on press: mcp
                    .stop(
                        mcp.server,
                        session: mcp.AgentSessionRef(grant: grant.id, session: s.agent.name),
                    )
                    .then(
                        if let last = s.actions.last() {
                            if let kept = last.target.app_id {
                                history.open_change(
                                    history.history,
                                    app_id: .some(kept),
                                    id: last.target.id,
                                    kind: last.target.kind,
                                    label: last.target.label,
                                )
                            } else {
                                history.open_change(
                                    history.history,
                                    id: last.target.id,
                                    kind: last.target.kind,
                                    label: last.target.label,
                                )
                            }
                        } else if let ws = workspace_of_session(s, server) {
                            session.open_link(link: ws.open)
                        } else {
                            history.open_change(
                                history.history,
                                id: s.id,
                                kind: "session",
                                label: s.instruction,
                            )
                        },
                    )
            }
        }
        ui.Button(
            "Undo session",
            icon: .undo_2,
            enabled: s.undo_blocked == .none,
            description: s.undo_blocked ?? "It can be undone here",
        ) {
            on press: history.undo_session(history.history, from: s.range.from, to: s.range.to)
        }
    }
}

/// The session's actions, oldest first, the one `opened` names with its view under it.
view timeline(s: history.AgentSession, opened: State<Text>, full: State<Text>) {
    ui.Section(actions_title(s.actions.len())) {
        if s.actions.is_empty() {
            ui.EmptyState("It changed nothing yet", icon: .bot, inline: true)
        }
        for a in s.actions {
            let showing = opened == a.range.from
            ui.ListRow(
                title: a.summary,
                secondary: if a.target.label != "" { a.target.label } else { a.place },
                meta: when(a.at_ms),
                icon: if a.kind == "file" {
                    .file_text
                } else if a.kind == "command" {
                    .terminal
                } else if a.kind == "page" { .globe } else { .app_window },
                selected: showing,
            ) {
                on activate: set(opened, if showing { "" } else { a.range.from })
                    .then(set(full, ""))
                trailing: {
                    if let label = a.undo_label {
                        ui.Button(label, role: .plain) {
                            on press: if a.undo_verb == .some("undo-entry") {
                                history.undo_record(
                                    history.history,
                                    range: history.Span(from: a.range.from, to: a.range.to),
                                    app_id: a.app_id,
                                    id: a.record,
                                )
                            } else {
                                history.undo_change(
                                    history.history,
                                    from: a.range.from,
                                    to: a.range.to,
                                )
                            }
                        }
                    }
                }
            }
            if showing {
                action_view(a, full)
            }
        }
    }
}

/// What one action shows when it is opened: never just its sentence.
view action_view(a: history.SessionAction, full: State<Text>) {
    stack(gap: .related) {
        if a.kind == "file" {
            ui.Text(a.place, role: .code)
            if a.diff.is_empty() {
                compared(a)
            } else {
                diff_lines(a)
            }
        } else if a.kind == "command" {
            if a.cwd != "" {
                ui.Text("in {a.cwd}", role: .caption, secondary: true)
            }
            if let execution = a.execution {
                if a.more {
                    output(a, execution, full)
                } else {
                    command_lines(a)
                }
            } else {
                command_lines(a)
            }
            command_status(a)
        } else if a.kind == "page" {
            ui.Text(if a.title != "" { a.title } else { a.target.label }, role: .label)
            ui.Text(a.place, role: .code)
        } else {
            row(gap: .related, align: .center) {
                ui.Text(a.target.label, role: .body)
                if let label = a.target.app_label {
                    ui.Text(label, role: .caption, secondary: true)
                }
                ui.Text(a.target.id, role: .caption, secondary: true)
            }
            compared(a)
        }
        ui.Button("Open", icon: .external_link) {
            on press: if let kept = a.target.app_id {
                history.open_change(
                    history.history,
                    app_id: .some(kept),
                    id: a.target.id,
                    kind: a.target.kind,
                    label: a.target.label,
                )
            } else {
                history.open_change(
                    history.history,
                    id: a.target.id,
                    kind: a.target.kind,
                    label: a.target.label,
                )
            }
        }
    }
}

/// A command's output: its last lines, or every line once the person asks, read from the session's
/// own reading of that execution.
view output(a: history.SessionAction, execution: history.Execution, full: State<Text>) {
    let all = full == a.range.from
    if all {
        let workspace = execution.workspace ?? "-"
        let reading = resource(
            "workspace.activity/output/{workspace}/{execution.app_id}/{execution.record}/{execution.execution}",
            of: history.OutputReading,
        )
        if let whole = reading.output {
            if whole.unavailable {
                ui.Text(
                    "Its output has not reached this device yet.",
                    role: .caption,
                    secondary: true,
                )
            } else {
                for line in whole.lines {
                    ui.Text(line, role: .code)
                }
                if whole.truncated {
                    ui.Text("It printed more than its shell kept.", role: .caption, secondary: true)
                }
            }
        } else {
            ui.Text("Reading every line…", role: .caption, secondary: true)
        }
    } else {
        command_lines(a)
    }
    ui.Button(if all { "Show less" } else { "Show all output" }, role: .plain) {
        on press: set(full, if all { "" } else { a.range.from })
    }
}

/// A command's last lines, with the prompt line they follow.
view command_lines(a: history.SessionAction) {
    ui.Text("$ {a.place}", role: .code)
    for line in a.output {
        ui.Text(line, role: .code)
    }
}

/// How a command ended: still running, or its exit status and how long it took, a failure in the
/// danger ink.
view command_status(a: history.SessionAction) {
    if a.exit == .none {
        if a.execution != .none {
            ui.Badge("Running…", tone: .info)
        }
    } else if let code = a.exit {
        // The action's own time is when its shell started the command, so the two say how long
        // it ran.
        let said = if let ended = a.ended_ms {
            if ended > a.at_ms {
                let took = Duration((ended - a.at_ms) / 1000)
                "Exited with {code} after {took}"
            } else { "Exited with {code}" }
        } else { "Exited with {code}" }
        if code == 0 {
            ui.Text(said, role: .caption, secondary: true)
        } else {
            ui.Badge(said, tone: .danger)
        }
    }
}

/// A file's change, line by line: what it removed and what it added, marked and inked.
view diff_lines(a: history.SessionAction) {
    for line in a.diff {
        if line.side == "removed" {
            ui.Text("− {line.text}", role: .code, secondary: true)
        } else if line.side == "added" {
            ui.Text("+ {line.text}", role: .code)
        } else {
            ui.Text("  {line.text}", role: .code, secondary: true)
        }
    }
}

/// What a change did to the workspace, as the history compares it either side of the action: each
/// field before and after, or else each line that moved.
view compared(a: history.SessionAction) {
    let reading = resource(
        "workspace.activity/compare/{a.range.from}-{a.range.to}",
        of: history.RangeDetail,
    )
    if let detail = reading.detail {
        if not detail.changes.is_empty() {
            for change in detail.changes {
                ui.Text(change.field, role: .caption)
                if change.before != "" {
                    ui.Text("− {change.before}", role: .code, secondary: true)
                }
                if change.after != "" {
                    ui.Text("+ {change.after}", role: .code)
                }
            }
        } else if let comparison = detail.comparison {
            ui.Text(comparison, role: .code)
        } else {
            ui.Text("This change shows no difference to preview.", role: .caption, secondary: true)
        }
    } else {
        ui.Text("Reading what changed", role: .caption, secondary: true)
    }
}

/// The card of one client: what it is doing, its commands, and the access it came in by.
view card(
    server: mcp.Server,
    sessions: history.Sessions,
    client: Text,
    session: State<Text>,
    agent: State<Text>,
    opened: State<Text>,
    full: State<Text>,
) {
    let of_client = sessions.sessions
        .filter(one -> client_of(one.agent.name) == client)
        .sort_by(one -> one.last_ms, desc)
    let newest = of_client.first()
    let grant = grant_of(server, client)
    stack(gap: .section) {
        ui.Button("Back", icon: .arrow_left) {
            on press: set(agent, "").then(set(session, ""))
        }
        ui.ObjectHeader(
            title: client,
            caption: if let one = newest {
                if working(one, sessions.sessions, server) and one.now != "" {
                    now_line(one)
                } else { "Last worked {when(one.last_ms)}" }
            } else { "No sessions yet" },
            square: true,
        )
        if let one = newest {
            commands(one, sessions, server)
            if let blocked = one.undo_blocked {
                ui.Text(blocked, role: .caption, secondary: true)
            }
        }
        if let g = grant {
            ui.Section("Access") {
                ui.ListRow(title: reach_line(g), secondary: grant_status(g), icon: .link) {
                    trailing: {
                        if g.stopped {
                            ui.Button("Let it go on") {
                                on press: mcp.go_on(
                                    mcp.server,
                                    session: mcp.AgentSessionRef(grant: g.id, session: g.agent),
                                )
                            }
                        } else {
                            ui.Button("Revoke", role: .destructive) {
                                on press: device.revoke(device.devices, device: g.id)
                            }
                        }
                    }
                }
            }
        }
        if of_client.is_empty() {
            ui.EmptyState("No sessions yet", icon: .bot)
        } else {
            ui.Section("Sessions") {
                for one in of_client {
                    session_row(one, sessions, server, session, opened)
                }
            }
        }
    }
}

/// Every client one of the sessions or grants names, each once, in the order they come.
fn clients_of(sessions: history.Sessions, server: mcp.Server) -> List<Text> {
    let out: List<Text> = []
    for one in sessions.sessions {
        let client = client_of(one.agent.name)
        if client != "" and not out.contains(client) {
            out = out + [client]
        }
    }
    for g in server.grants {
        let client = client_of(g.agent)
        if client != "" and not out.contains(client) {
            out = out + [client]
        }
    }
    out
}

/// One section listing the clients that acted here, each opening its card.
view clients_section(
    server: mcp.Server,
    sessions: history.Sessions,
    agent: State<Text>,
    session: State<Text>,
) {
    let clients = clients_of(sessions, server)
    if not clients.is_empty() {
        ui.Section("Clients") {
            for client in clients {
                let of_client = sessions.sessions
                    .filter(one -> client_of(one.agent.name) == client)
                    .sort_by(one -> one.last_ms, desc)
                ui.ListRow(
                    title: client,
                    secondary: if let one = of_client.first() {
                        if one.now != "" and working(one, sessions.sessions, server) {
                            now_line(one)
                        } else { "Last worked {when(one.last_ms)}" }
                    } else { "Connected, no sessions yet" },
                    icon: if grant_of(server, client) != .none { .link } else { .bot },
                ) {
                    on activate: set(agent, client).then(set(session, ""))
                }
            }
        }
    }
}

/// Workspaces: the person's workspaces the MCP server reaches, each one step from the full
/// desktop.
view workspaces(server: mcp.Server) {
    stack(gap: .section) {
        if server.workspaces.is_empty() {
            ui.EmptyState(
                "No workspaces yet",
                message: "Make one in Tilo, or ask an agent to make one for its work.",
                icon: .layout_grid,
            )
        } else {
            ui.Section("Workspaces") {
                for ws in server.workspaces {
                    ui.ListRow(
                        title: ws.name,
                        note: made_by(ws),
                        meta: when(ws.made_ms),
                        icon: .layout_grid,
                    ) {
                        trailing: {
                            ui.Button("Open in Tilo", icon: .external_link) {
                                on press: session.open_link(link: ws.open)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Who made a workspace, as its row says it.
fn made_by(ws: mcp.Workspace) -> Text {
    if let made = ws.made_by {
        if made.kind == "person" {
            std.say("Made by you")
        } else if let agent = made.agent { std.say("Made by {agent}") } else { "" }
    } else { "" }
}

/// Connect: where an MCP client reaches the person's workspaces, and every agent the server let
/// in, with Revoke.
view connect(server: mcp.Server) {
    stack(gap: .section) {
        if server.endpoint == "" {
            stack(gap: .related) {
                ui.Text(
                    "Your agent works here as you once Tilo is set up on its computer. One line you paste into it does that and signs it in as you.",
                )
                ui.Button("Set up your agent", role: .primary) {
                    on press: session.show_connect()
                }
            }
        } else {
            ui.Text(
                "Add this server to the MCP configuration of your agent. It asks you to sign in once, then works in your workspaces as itself. Everything it does shows under Agents, where you can stop or undo it.",
            )
            ui.Section("Connect an agent") {
                ui.PropertyList {
                    ui.Property("Server address", server.endpoint, copyable: true)
                    ui.Property("Any MCP client", client_config(server.endpoint), copyable: true)
                }
            }
            ui.Section("Connected agents") {
                if server.grants.is_empty() {
                    ui.EmptyState("No agent connected yet", icon: .link)
                }
                for g in server.grants {
                    ui.ListRow(
                        title: g.agent,
                        secondary: if g.browser_pages {
                            "{reach_line(g)}, and operates web pages"
                        } else { reach_line(g) },
                        meta: grant_status(g),
                        icon: .link,
                    ) {
                        trailing: {
                            if g.stopped {
                                ui.Button("Let it go on") {
                                    on press: mcp.go_on(
                                        mcp.server,
                                        session: mcp.AgentSessionRef(grant: g.id, session: g.agent),
                                    )
                                }
                            } else {
                                ui.Button("Revoke", role: .destructive) {
                                    on press: device.revoke(device.devices, device: g.id)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The MCP configuration an agent adds to reach the server, as one line for any client.
fn client_config(endpoint: Text) -> Text {
    "\{\"mcpServers\": \{\"tilo\": \{\"type\": \"http\", \"url\": \"{endpoint}\"\}\}\}\}"
}

/// Where a grant lets the agent work, in words.
fn reach_line(g: mcp.Grant) -> Text {
    if g.reach.kind == "workspace" {
        if let name = g.reach.name { std.say("Only {name}") } else { std.say("Only one workspace") }
    } else {
        std.say("Every workspace, and may make new ones")
    }
}

/// How a grant stands, as its row says it: how many sessions work now, or when it last acted.
fn grant_status(g: mcp.Grant) -> Text {
    let working = g.sessions.len()
    if working == 1 {
        std.say("Working now")
    } else if working > 1 {
        std.say("{working} sessions working now")
    } else if let last = g.last_ms {
        std.say("Last active {when(last)}")
    } else { std.say("Let in {when(g.granted_ms)}") }
}

/// Usage: how long the agents' Linux machines ran.
view usage() {
    stack(gap: .section) {
        ui.EmptyState(
            "No Linux machine time yet",
            message: "An agent that needs Linux runs it on a machine of its own, and that time is counted.",
            icon: .gauge,
        )
    }
}

/// What a working session is doing now, as its row says it.
fn now_line(s: history.AgentSession) -> Text {
    if s.now_kind == "running" {
        std.say("Running {s.now}…")
    } else if s.now_kind == "editing" {
        std.say("Editing {s.now}")
    } else if s.now_kind == "browsing" {
        std.say("On {s.now}")
    } else if s.now_kind == "ran" { std.say("Ran {s.now}") } else { s.now }
}

/// Who started a session, as its row says it: "Started by you" for the reader's own, the
/// starter's name for someone else's.
fn started_by(a: history.Author) -> Text {
    if a.for_reader {
        std.say("Started by you")
    } else if let name = a.on_behalf_of {
        if name.trim().is_empty() { a.name } else { std.say("Started by {name}") }
    } else { a.name }
}

fn actions_title(count: Int) -> Text {
    if count == 1 { std.say("1 action") } else { std.say("{count} actions") }
}

/// How much a session did: the changes it made, else the actions it took.
fn size_line(s: history.AgentSession) -> Text {
    let changes = changes_of(s)
    if changes == 0 {
        actions_title(s.actions.len())
    } else if changes == 1 { std.say("1 change") } else { std.say("{changes} changes") }
}

fn changes_of(s: history.AgentSession) -> Int {
    let total = 0
    for a in s.actions {
        if a.kind == "file" or a.kind == "record" {
            total = total + if a.changes < 1 { 1 } else { a.changes }
        }
    }
    total
}

/// The sessions `agent` and `workspace` leave: a name picks the session of that client, and the
/// workspace narrows to it.
fn shows(s: history.AgentSession, agent: Text, workspace: Text) -> Bool {
    let named = agent == "" or agent == s.agent.name or agent == client_of(s.agent.name)
    named and (workspace == "" or s.workspace == .some(workspace))
}

/// The client `name` comes from: a run is named after its client and a number of the person's
/// runs ("Claude Code #3"), a grant carries the client's own name.
fn client_of(name: Text) -> Text {
    let trimmed = name.trim()
    let parts = trimmed.split(" ")
    let tail = parts.last() ?? ""
    if parts.len() < 2 or not tail.starts_with("#") {
        return trimmed
    }
    let digits = tail.slice(1, tail.len())
    if digits.is_empty() or digits.to_int() == .none {
        return trimmed
    }
    parts.take(parts.len() - 1).join(" ").trim()
}

/// Whether it is working now: its agent session through the MCP server is open and it is that
/// session's newest work.
fn working(
    s: history.AgentSession,
    sessions: List<history.AgentSession>,
    server: mcp.Server,
) -> Bool {
    if not server.grants.any(g -> g.sessions.any(current -> current.name == s.agent.name)) {
        return false
    }
    let newest = sessions
        .filter(other -> other.agent.name == s.agent.name)
        .sort_by(other -> other.last_ms, desc)
        .first()
    if let newest = newest { newest.id == s.id } else { false }
}

/// The grant a session came in by, when the server lists one.
fn open_grant(s: history.AgentSession, server: mcp.Server) -> Option<mcp.Grant> {
    server.grants.find(g -> g.sessions.any(current -> current.name == s.agent.name))
}

/// The grant `client` came in by, when the server lists one.
fn grant_of(server: mcp.Server, client: Text) -> Option<mcp.Grant> {
    server.grants.find(g -> client_of(g.agent) == client)
}

fn workspace_of_session(s: history.AgentSession, server: mcp.Server) -> Option<mcp.Workspace> {
    if let name = s.workspace {
        server.workspaces.find(w -> w.name == name or w.id == name)
    } else { .none }
}

/// When `at_ms` was: the day and the clock time.
fn when(at_ms: Int) -> Text {
    let at = moment(at_ms)
    "{at.day()}, {at.clock()}"
}

/// The instant a count of milliseconds names.
fn moment(at_ms: Int) -> Time {
    Time("1970-01-01T00:00:00Z") + Duration(at_ms / 1000)
}

fn workspaces_of(sessions: history.Sessions) -> List<Text> {
    let out: List<Text> = []
    for s in sessions.sessions {
        if let name = s.workspace {
            if not out.contains(name) {
                out = out + [name]
            }
        }
    }
    out
}
