app so.tilo.lang.workspace-settings "Workspace settings" (
    icon: .settings,
    category: .system,
    tagline: "This workspace and who works in it",
    keywords: ["workspace", "members", "agents", "access", "arbeitsbereich", "mitglieder", "agenten", "einstellungen"],
)

use so.tilo.assistant as assistant

/// The workspace this window looks after: who works in it and what each of them may do. It keeps
/// nothing of its own here: the agents are the workspace's record, read as `assistant.agents`,
/// and everything the window changes is an action of that record, so a change made here, by
/// another member or by an agent is one change, with one history.
type WorkspaceSettings {
}

root WorkspaceSettings

default view workspace_settings for WorkspaceSettings

/// The window: the parts of the workspace down the side, the chosen one beside them.
view workspace_settings(w: WorkspaceSettings) {
    state tab: Text = "agents"
    ui.NavShell(content_kind: .page, title: "Agents", sidebar_title: "Workspace settings") {
        sidebar: {
            ui.NavList(selected: tab) {
                ui.NavSection("Members") {
                    ui.NavItem("agents", "Agents", icon: .sparkles) {
                        on select: set(tab, "agents")
                    }
                }
            }
        }
        content: {
            ui.Scroll {
                stack(padding: .group, max: 64rem, gap: .section) {
                    agents_pane()
                }
            }
        }
    }
}

/// The agents that work in the workspace, and one agent's page when the person opened one: its
/// name, whether it works, and a switch that pauses or resumes it. The switch asks the record's
/// own `pause-agent` or `resume-agent`, so what a person presses here, an agent asked for the
/// same change and another device all take one path; whoever the workspace's grants do not let
/// change its agents sees the switch disabled, with why.
view agents_pane() {
    state reviewing: Text = ""
    let a = assistant.agents
    let p = assistant.agent_permissions
    let opened = a.agents.find(x -> x.id == reviewing)
    if let agent = opened {
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("All agents", icon: .arrow_left, size: .small) {
                on press: set(reviewing, "")
            }
            ui.Text(agent.name, role: .title, lines: 1)
        }
        ui.Text(agent_words(agent), role: .caption, secondary: true)
        if agent.state != .retired {
            let may = if agent.state == .active { p.pause } else { p.resume }
            ui.Section("Working") {
                ui.Toggle(
                    shown: agent.state == .active,
                    label: std.say("Working"),
                    enabled: may,
                ) {
                    on check(working): if working {
                        assistant.resume_agent(assistant.agents, agent: agent.id)
                    } else {
                        assistant.pause_agent(assistant.agents, agent: agent.id)
                    }
                }
                if agent.state == .paused {
                    ui.Text(
                        std.say("Paused: it takes no new work until it is resumed."),
                        role: .caption,
                        secondary: true,
                    )
                } else {
                    ui.Text(
                        std.say("Takes work from its triggers and conversations."),
                        role: .caption,
                        secondary: true,
                    )
                }
                if not may {
                    ui.Text(
                        std.say("Only members who may change the workspace's agents pause or resume one."),
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
    } else {
        ui.Section("Agents") {
            if a.agents.is_empty() {
                ui.EmptyState(
                    "No agent works in this workspace yet.",
                    icon: .sparkles,
                    inline: true,
                )
            }
            for agent in a.agents {
                ui.ListRow(agent.name, secondary: agent_words(agent), icon: .sparkles) {
                    on activate: set(reviewing, agent.id)
                }
            }
        }
    }
}

/// What an agent is, in a line: whether it works, and the model it plans with.
fn agent_words(agent: assistant.Agent) -> Text {
    let state = if agent.state == .active {
        std.say("Working")
    } else if agent.state == .paused {
        std.say("Paused")
    } else {
        std.say("Retired")
    }
    if let model = agent.model {
        "{state}, {model}"
    } else {
        state
    }
}
