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

/// One agent session, as a resource of its own: opening it shows its timeline in a window of its
/// own, from a list, search, a link or another app.
type SessionLink (title: instruction, untitled: "Agent session") {
    /// The history's own name for the run.
    run: Text
    /// What the session was asked, as the history last listed it: what its window is called.
    instruction: Text = ""
}

/// One client, as a resource of its own: opening it shows its card in a window of its own.
type AgentCard (title: client) {
    /// The client's name, as its runs and its grant say it.
    client: Text
}

/// The window: which place it opens in, and the sessions and clients it has heard of, each a
/// resource of its own. Everything else it shows is what the person narrowed while it ran, which
/// is the window's own and never kept.
type Window {
    /// The place the window opens in: `agents`, `workspaces`, `connect` or `usage`.
    place: Text = "agents"
    /// The sessions the window has given a resource of their own: the newest the history listed
    /// as it read it, and every older one a link was followed to since.
    sessions: Collection<SessionLink>
    /// The clients that acted here, each a resource of its own: its card.
    cards: Collection<AgentCard>
}

root window: Window

default view console for Window

default view session_window for SessionLink

default view client_card for AgentCard

/// The window: the four places down one side, and the place shown beside them. Agents is the
/// person's own work seen through what their agents did: every session of every agent across the
/// workspaces this device holds, and the clients that acted here.
view console(w: Window) {
    // Empty until the person chooses a place: the window opens where the root says, so an action
    // that sets it (`open_connect`) lands there, and the person's choice wins from then on.
    state place: Text = ""
    let shown = if place == "" { w.place } else { place }
    state agent_filter: Text = ""
    state workspace_filter: Text = ""
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    // The window reads the history and the server: what they list gets a resource of its own,
    // while the window stands open, so a session or a client can be linked to, found and opened
    // outside it.
    if behind(w, sessions, server) {
        every 1s: publish(w)
    }
    ui.NavShell(content_kind: .page, title: place_title(shown), sidebar_title: "Console") {
        sidebar: {
            ui.NavList(selected: shown) {
                ui.NavItem("agents", "Agents", icon: .bot) {
                    on select: set(place, "agents")
                }
                ui.NavItem("workspaces", "Workspaces", icon: .layout_grid) {
                    on select: set(place, "workspaces")
                }
                ui.NavItem("connect", "Connect", icon: .link) {
                    on select: set(place, "connect")
                }
                ui.NavItem("usage", "Usage", icon: .gauge) {
                    on select: set(place, "usage")
                }
            }
        }
        content: {
            if shown == "agents" {
                agents(w, sessions, server, agent_filter, workspace_filter)
            } else if shown == "workspaces" {
                workspaces(server)
            } else if shown == "connect" {
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

/// Gives each of the newest sessions the history lists, and each client that acted here, a
/// resource of its own, so each can be linked to, found and opened outside this window. An
/// insert under a key a resource already stands under leaves that resource as it stands, so a
/// reading that answers again changes nothing twice.
action publish(w: Window) {
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    for s in sessions.sessions.take(kept_sessions()) {
        let link = w.sessions.insert(SessionLink(run: s.id, instruction: s.instruction), key: s.id)
        if link.instruction != s.instruction {
            link.instruction = s.instruction
        }
    }
    for client in clients_of(sessions, server) {
        w.cards.insert(AgentCard(client: client), key: client)
    }
}

/// The resource of the session `run`, made now when the window has not published it yet: every
/// way of opening a session goes through it, so the list, a link, search and another app open
/// the same window.
action session_link(w: Window, run: Text) -> SessionLink {
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let instruction = if let s = sessions.sessions.find(one -> one.id == run) {
        s.instruction
    } else { "" }
    let link = w.sessions.insert(SessionLink(run: run, instruction: instruction), key: run)
    if instruction != "" and link.instruction != instruction {
        link.instruction = instruction
    }
    link
}

/// The card of `client`, made now when the window has not published it yet: every way of opening
/// a client's card goes through it.
action agent_card(w: Window, client: Text) -> AgentCard {
    w.cards.insert(AgentCard(client: client), key: client)
}

/// Stops the client's working session, keeping its grant.
destructive action stop_agent(c: AgentCard) {
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    if let s = newest_of(sessions, c.client) {
        if let grant = open_grant(s, server) {
            if working(s, sessions.sessions, server) {
                mcp.stop(
                    mcp.server,
                    session: mcp.AgentSessionRef(grant: grant.id, session: s.agent.name),
                )
            }
        }
    }
}

/// What an action of a session changed, as the app opens it.
type Opening {
    /// The app it opens in, when its view names one.
    app_id: Option<Text> = .none
    /// The resource: its id, its kind and its title.
    id: Text
    kind: Text
    label: Text
}

/// Where an action's view opens, as its own record.
fn opening(v: history.ActionView) -> Opening {
    let target = action_target(v)
    Opening(app_id: target.app_id, id: target.id, kind: target.kind, label: target.label)
}

/// Opens what an action of a session changed, in its app.
action open_action(o: Opening) {
    if let kept = o.app_id {
        history.open(history.history, app_id: .some(kept), id: o.id, kind: o.kind, label: o.label)
    } else {
        history.open(history.history, id: o.id, kind: o.kind, label: o.label)
    }
}

/// Stops the client's session and opens what it changed last, or the workspace it worked in.
action take_over_agent(c: AgentCard) {
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    if let s = newest_of(sessions, c.client) {
        if let grant = open_grant(s, server) {
            mcp.stop(
                mcp.server,
                session: mcp.AgentSessionRef(grant: grant.id, session: s.agent.name),
            )
            if let last = s.actions.last() {
                open_action(opening(last.view))
            } else if let ws = workspace_of_session(s, server) {
                device.open_link(device.invite, link: ws.open)
            } else {
                history.open(history.history, id: s.id, kind: "session", label: s.instruction)
            }
        }
    }
}

/// Takes back every change of the client's newest session, when the history says it can.
destructive action undo_agent_session(c: AgentCard) {
    require can_undo(c) undo_blocked(c)
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    if let s = newest_of(sessions, c.client) {
        history.undo_session(history.history, from: s.range.from, to: s.range.to)
    }
}

/// Takes back the grant the client came in by.
destructive action revoke_agent(c: AgentCard) {
    let server = mcp.server
    if let grant = grant_of(server, c.client) {
        device.revoke(device.devices, device: grant.id)
    }
}

/// Lets a stopped client's calls through again.
action let_agent_go_on(c: AgentCard) {
    let server = mcp.server
    if let grant = grant_of(server, c.client) {
        mcp.go_on(mcp.server, session: mcp.AgentSessionRef(grant: grant.id, session: grant.agent))
    }
}

/// Stops the client's working session, keeping its grant: the card's own Stop, by the client a
/// card, a palette or an agent names.
action stop_client(w: Window, client: Text) {
    stop_agent(agent_card(w, client))
}

/// Stops the client's session and opens what it changed last: the card's own Take over, by the
/// client a card, a palette or an agent names.
action take_over_client(w: Window, client: Text) {
    take_over_agent(agent_card(w, client))
}

/// Takes back every change of the client's newest session: the card's own Undo session, by the
/// client a card, a palette or an agent names.
destructive action undo_client_session(w: Window, client: Text) {
    undo_agent_session(agent_card(w, client))
}

/// Takes back the grant the client came in by: the card's own Revoke, by the client a card, a
/// palette or an agent names.
destructive action revoke_client(w: Window, client: Text) {
    revoke_agent(agent_card(w, client))
}

/// Lets a stopped client's calls through again: the card's own Let it go on, by the client a
/// card, a palette or an agent names.
action let_client_go_on(w: Window, client: Text) {
    let_agent_go_on(agent_card(w, client))
}

/// Shows how an agent or another computer connects as this person: the window opens at Connect.
action open_connect(w: Window) {
    w.place = "connect"
}

/// Agents: every session across the workspaces, narrowed by the agent and the workspace the
/// person chose, and the clients that acted here.
view agents(
    w: Window,
    sessions: history.Sessions,
    server: mcp.Server,
    agent_filter: State<Text>,
    workspace_filter: State<Text>,
) {
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
                    session_row(s, sessions, server, w)
                }
            }
        }
        if not earlier.is_empty() {
            ui.Section("Earlier") {
                for s in earlier {
                    session_row(s, sessions, server, w)
                }
            }
        }
        clients_section(server, sessions, w)
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
/// did, in which workspace, and when. Activating it opens the session's own resource.
view session_row(
    s: history.AgentSession,
    sessions: history.Sessions,
    server: mcp.Server,
    w: Window,
) {
    let is_working = working(s, sessions.sessions, server)
    ui.ListRow(
        title: s.instruction,
        note: if is_working and s.now != .none { now_line(s) } else { started_by(s.agent) },
        secondary: s.workspace ?? size_line(s),
        meta: when(s.last_ms),
        icon: if s.agent.relation == .software { .bot } else { .file_text },
    ) {
        on activate: {
            let link = session_link(w, s.id)
            open(link)
        }
    }
}

/// A session as its own window: its page once the history lists it, or the state that says why
/// there is none yet. A link names the session before the history has answered, so the page
/// says it is reading and fills in when the answer lands.
view session_window(link: SessionLink) {
    state opened: Text = ""
    state full: Text = ""
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    if let s = sessions.sessions.find(one -> one.id == link.run) {
        session_page(s, sessions, server, link, opened, full)
    } else if sessions.pending {
        ui.EmptyState("Reading this session", icon: .clock)
    } else {
        ui.EmptyState(
            "This session is not here",
            message: "The history lists no such session, or it worked in a workspace you are not in.",
            icon: .bot,
        )
    }
}

/// The session chosen: what it was asked, what can be done with it, why it did what it did, and
/// its actions, each opening to what it did.
view session_page(
    s: history.AgentSession,
    sessions: history.Sessions,
    server: mcp.Server,
    link: SessionLink,
    opened: State<Text>,
    full: State<Text>,
) {
    stack(gap: .section) {
        ui.ObjectHeader(title: s.instruction, caption: started_by(s.agent))
        commands(s, sessions, server, link)
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

/// Stop, Take over, Undo and the session's link, as the session allows them.
view commands(
    s: history.AgentSession,
    sessions: history.Sessions,
    server: mcp.Server,
    link: SessionLink,
) {
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
                            open_action(opening(last.view))
                        } else if let ws = workspace_of_session(s, server) {
                            device.open_link(device.invite, link: ws.open)
                        } else {
                            history.open(
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
        // The system's Copy link on the session's own resource: the link that opens this
        // session's page for the people and devices of the workspace.
        ui.Button("Copy link", icon: .link) {
            on press: link.share_link()
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
            let target = action_target(a.view)
            ui.ListRow(
                title: a.summary,
                secondary: if target.label != "" { target.label } else { action_place(a.view) },
                meta: when(a.at_ms),
                icon: acted_on(a.view).icon(),
                selected: showing,
            ) {
                on activate: set(opened, if showing { "" } else { a.range.from })
                    .then(set(full, ""))
                trailing: {
                    if let undo = a.undo {
                        ui.Button(undo.label, role: .plain) {
                            on press: if undo.action == .undo-entry {
                                history.undo_record(
                                    history.history,
                                    range: history.Range(from: a.range.from, to: a.range.to),
                                    app_id: a.app_id,
                                    id: a.record,
                                )
                            } else {
                                history.undo(history.history, from: a.range.from, to: a.range.to)
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

/// The resource an action's view touched, where it opens.
fn action_target(v: history.ActionView) -> history.Target {
    match v {
        .file(target, _path, _lines) => target,
        .command(target, _command, _cwd, _output, _more, _exit, _started, _ended, _execution) => target,
        .page(target, _url, _title) => target,
        .record(target) => target,
    }
}

/// What an action's view did, as its row's glyph says it.
enum ActedOn {
    file (icon: .file_text)
    command (icon: .terminal)
    page (icon: .globe)
    record (icon: .app_window)
}

/// The kind of thing an action's view touched.
fn acted_on(v: history.ActionView) -> ActedOn {
    match v {
        .file => .file,
        .command => .command,
        .page => .page,
        .record => .record,
    }
}

/// Where an action landed, as its row names it: a file's path, a command line, a page's
/// address, or a record's id.
fn action_place(v: history.ActionView) -> Text {
    match v {
        .file(_target, path, _lines) => path,
        .command(_target, command, _cwd, _output, _more, _exit, _started, _ended, _execution) => command,
        .page(_target, url, _title) => url,
        .record(target) => target.id,
    }
}

/// What one action shows when it is opened: never just its sentence.
view action_view(a: history.SessionAction, full: State<Text>) {
    stack(gap: .related) {
        match a.view {
            .file(_target, path, lines) => {
                ui.Text(path, role: .code)
                if lines.is_empty() {
                    compared(a)
                } else {
                    diff_lines(lines)
                }
            }
            .command(_target, command, cwd, output, more, exit, _started, ended, execution) => {
                if cwd != "" {
                    ui.Text("in {cwd}", role: .caption, secondary: true)
                }
                if let place = execution {
                    if more {
                        command_output(command, output, place, a.range.from, full)
                    } else {
                        command_lines(command, output)
                    }
                } else {
                    command_lines(command, output)
                }
                command_status(exit, ended, a.at_ms, execution)
            }
            .page(target, url, title) => {
                ui.Text(if title != "" { title } else { target.label }, role: .label)
                ui.Text(url, role: .code)
            }
            .record(target) => {
                row(gap: .related, align: .center) {
                    ui.Text(target.label, role: .body)
                    if let label = target.app_label {
                        ui.Text(label, role: .caption, secondary: true)
                    }
                    ui.Text(target.id, role: .caption, secondary: true)
                }
                compared(a)
            }
        }
        let target = action_target(a.view)
        ui.Button("Open", icon: .external_link) {
            on press: if let kept = target.app_id {
                history.open(
                    history.history,
                    app_id: .some(kept),
                    id: target.id,
                    kind: target.kind,
                    label: target.label,
                )
            } else {
                history.open(history.history, id: target.id, kind: target.kind, label: target.label)
            }
        }
    }
}

/// A command's output: its last lines, or every line once the person asks, read from the session's
/// own reading of that execution.
view command_output(
    command: Text,
    lines: List<Text>,
    execution: history.CommandExecution,
    key: Text,
    full: State<Text>,
) {
    let all = full == key
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
        command_lines(command, lines)
    }
    ui.Button(if all { "Show less" } else { "Show all output" }, role: .plain) {
        on press: set(full, if all { "" } else { key })
    }
}

/// A command's last lines, with the prompt line they follow.
view command_lines(command: Text, lines: List<Text>) {
    ui.Text("$ {command}", role: .code)
    for line in lines {
        ui.Text(line, role: .code)
    }
}

/// How a command ended: still running, or its exit status and how long it took, a failure in the
/// danger ink.
view command_status(
    exit: Option<Int>,
    ended: Option<Int>,
    at_ms: Int,
    execution: Option<history.CommandExecution>,
) {
    if exit == .none {
        if let _ = execution {
            ui.Badge("Running…", tone: .info)
        }
    } else if let code = exit {
        // The action's own time is when its shell started the command, so the two say how long
        // it ran.
        let said = if let stopped = ended {
            if stopped > at_ms {
                let took = Duration((stopped - at_ms) / 1000)
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
view diff_lines(lines: List<history.DiffLine>) {
    for line in lines {
        if line.side == .removed {
            ui.Text("− {line.text}", role: .code, secondary: true)
        } else if line.side == .added {
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
            match comparison {
                .changes(lines) => {
                    ui.Text(lines.join("\n"), role: .code)
                }
                .unavailable => {
                    ui.Text(
                        "This change shows no difference to preview.",
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        } else {
            ui.Text("This change shows no difference to preview.", role: .caption, secondary: true)
        }
    } else {
        ui.Text("Reading what changed", role: .caption, secondary: true)
    }
}

/// The card of one client, as its own window: what it is doing, its commands, and the access it
/// came in by.
view client_card(c: AgentCard) {
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    let server = mcp.server
    let w = root Window
    let of_client = sessions.sessions
        .filter(one -> client_of(one.agent.name) == c.client)
        .sort_by(one -> one.last_ms, desc)
    let newest = of_client.first()
    let grant = grant_of(server, c.client)
    stack(gap: .section) {
        ui.ObjectHeader(
            title: c.client,
            caption: if let one = newest {
                if working(one, sessions.sessions, server) and one.now != .none {
                    now_line(one)
                } else { "Last worked {when(one.last_ms)}" }
            } else { "No sessions yet" },
            square: true,
        )
        row(gap: .related, wrap: true, align: .center) {
            // The system's Copy link on the card: the link that opens this client's card for the
            // people and devices of the workspace, minted where the resource is served.
            ui.Button("Copy link", icon: .link) {
                on press: c.share_link()
            }
        }
        if let one = newest {
            card_commands(c, one, sessions, server)
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
                                on press: let_agent_go_on(c)
                            }
                        } else {
                            ui.Button("Revoke", role: .destructive) {
                                on press: revoke_agent(c)
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
                    session_row(one, sessions, server, w)
                }
            }
        }
    }
}

/// The card's commands: Stop and Take over for what the client works on now, and the undo the
/// history offers its newest session. Each is an action of the card, so it can be run from its
/// buttons, the palette or an agent alike.
view card_commands(
    c: AgentCard,
    s: history.AgentSession,
    sessions: history.Sessions,
    server: mcp.Server,
) {
    row(gap: .related, wrap: true, align: .center) {
        if open_grant(s, server) != .none {
            if working(s, sessions.sessions, server) {
                ui.Button("Stop", icon: .square, role: .destructive) {
                    on press: stop_agent(c)
                }
            }
            ui.Button("Take over", icon: .hand) {
                on press: take_over_agent(c)
            }
        }
        ui.Button("Undo session", icon: .undo_2) {
            on press: undo_agent_session(c)
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
view clients_section(server: mcp.Server, sessions: history.Sessions, w: Window) {
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
                        if one.now != .none and working(one, sessions.sessions, server) {
                            now_line(one)
                        } else { "Last worked {when(one.last_ms)}" }
                    } else { "Connected, no sessions yet" },
                    icon: if grant_of(server, client) != .none { .link } else { .bot },
                ) {
                    on activate: {
                        let card = agent_card(w, client)
                        open(card)
                    }
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
                                on press: device.open_link(device.invite, link: ws.open)
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
                    on press: device.show_connect(device.invite)
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
    if let now = s.now {
        match now {
            .running(command, _since) => std.say("Running {command}…"),
            .editing(path, _at) => std.say("Editing {path}"),
            .browsing(url, title, _at) => {
                if title == "" { std.say("On {url}") } else { std.say("On {title}") }
            }
            .changing(summary, _at) => summary,
            .ran(command, _at) => std.say("Ran {command}"),
        }
    } else { "" }
}

/// Whether the action changed a file or a record, as a person counts what a session changed.
fn edited(v: history.ActionView) -> Bool {
    match v {
        .file => true,
        .record => true,
        .command => false,
        .page => false,
    }
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
        if edited(a.view) {
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

/// How many of the newest sessions the window gives a resource of their own: its list's first
/// page, as the history reads it.
fn kept_sessions() -> Int {
    16
}

/// Whether the history or the server lists a session or a client the window has not given a
/// resource of its own yet.
fn behind(w: Window, sessions: history.Sessions, server: mcp.Server) -> Bool {
    let unseen = sessions.sessions
        .take(kept_sessions())
        .any(s -> not w.sessions.any(link -> link.run == s.id))
    let unknown = clients_of(sessions, server).any(c -> not w.cards.any(card -> card.client == c))
    unseen or unknown
}

/// The client's newest session the history lists: the one its card's commands act on.
fn newest_of(sessions: history.Sessions, client: Text) -> Option<history.AgentSession> {
    sessions.sessions
        .filter(one -> client_of(one.agent.name) == client)
        .sort_by(one -> one.last_ms, desc)
        .first()
}

/// Why the card's undo cannot run now, as the history says it: empty while it can, and what its
/// button reads while it cannot.
fn undo_blocked(c: AgentCard) -> Text {
    let sessions = resource("workspace.activity/sessions/all", of: history.Sessions)
    if let s = newest_of(sessions, c.client) {
        s.undo_blocked ?? ""
    } else { std.say("It has no session to undo") }
}

/// Whether the history would take back the client's newest session.
fn can_undo(c: AgentCard) -> Bool {
    undo_blocked(c) == ""
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
