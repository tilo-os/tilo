/// The workspace in front: who is in it and what part each holds, the agents that work here, what
/// the workspace granted beyond those roles, and which app opens which kind of file. Every fact is
/// read from the workspace's own governance (`workspace.access`, the session's reading), and every
/// choice is that resource's own verb: a change here is the change an agent, a script and another
/// device make, decided by the workspace's rules whatever this page offered.
view workspace_pane() {
    let access = workspace.access
    if let gov = access.governance {
        ui.Section("People") {
            ui.Text(
                if gov.can_govern {
                    "Their role says what they may do here. The workspace is the owner's; its role is a fact, not a choice."
                } else {
                    "Their role says what they may do here. Changing one belongs to whoever governs this workspace."
                },
                role: .caption,
                secondary: true,
            )
            for person in gov.people {
                ui.ListRow(
                    person_name(person),
                    secondary: person_words(person),
                    icon: if person.owner { .key } else { .user },
                    selected: person.you,
                ) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            if person.owner {
                                ui.Chip("Owner", tone: .neutral)
                            } else if person.can_change {
                                ui.MenuButton(
                                    "Change {person_name(person)}'s role",
                                    icon: .ellipsis,
                                ) {
                                    for role in gov.roles {
                                        ui.MenuItem(role.name) {
                                            on choose: workspace.access.set_role(
                                                person: person.person,
                                                role: role.id,
                                            )
                                        }
                                    }
                                }
                            } else {
                                ui.Chip(person.role_name, tone: .neutral)
                            }
                            if person.can_remove {
                                ui.Button(
                                    "Remove {person_name(person)}",
                                    icon: .trash,
                                    icon_only: true,
                                    role: .destructive,
                                    size: .small,
                                ) {
                                    on press: workspace.access.remove_member(person: person.person)
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        ui.EmptyState(
            "No workspace is open",
            message: "The people and their roles belong to the workspace in front; open one and they show here.",
            icon: .users,
            inline: true,
        )
    }
    ui.Section("Agents") {
        let roster = workspace.agents
        std.readable(of: workspace.agents, message: "The workspace's agents could not be read") {
            if roster.agents.is_empty() {
                ui.Text("No agent works in this workspace yet.", role: .caption, secondary: true)
            }
            for agent in roster.agents.sort_by(a -> a.created_at) {
                ui.ListRow(agent.name, secondary: agent_words(agent), icon: .bot) {
                    trailing: {
                        ui.Chip(
                            agent_state_words(agent.state),
                            tone: if agent.state == .active { .success } else { .neutral },
                        )
                    }
                }
            }
            ui.Text(
                "An agent acts as the person it works for. What it may do here is the workspace's grants on it, listed under Permissions.",
                role: .caption,
                secondary: true,
            )
        }
    }
    ui.Section("Permissions") {
        ui.Text(
            "What the workspace grants beyond a person's role: each with what it allows and whom, and the way to take it back. Only an owner or an administrator may.",
            role: .caption,
            secondary: true,
        )
        let shown = permissions_of(access)
        if shown.is_empty() {
            ui.Text(
                "The workspace grants nothing beyond its roles and its own documents.",
                role: .caption,
            )
        }
        for grant in shown {
            ui.ListRow(grant.holder, secondary: grant_words(grant), icon: .shield) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.Chip(grant.section, tone: .neutral)
                        if grant.can_revoke {
                            ui.Button("Take it back", icon: .x, role: .destructive, size: .small) {
                                on press: workspace.access.revoke(id: grant.id)
                            }
                        }
                    }
                }
            }
        }
    }
    if let defaults = access.defaults {
        ui.Section("Default apps") {
            ui.Text(
                "Which app opens each kind of file or link here. The workspace's choice is for everyone; yours is yours alone.",
                role: .caption,
                secondary: true,
            )
            for entry in defaults {
                ui.ListRow(
                    entry.label,
                    secondary: "Here: {entry.workspace} · Yours: {entry.yours}",
                    icon: .layout_grid,
                ) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            ui.Button("Everyone", size: .small) {
                                on press: workspace.access.next_default(resource: entry.resource)
                            }
                            ui.Button("Mine", size: .small) {
                                on press: workspace.access.next_personal_default(
                                    resource: entry.resource,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
    if not access.known {
        ui.Text(
            "The workspace's access can't be read here yet; open the workspace and it shows.",
            role: .caption,
            secondary: true,
        )
    }
}

/// What a person is called here: their name, You for the person reading it, and their person id
/// where the workspace knows no name for them.
fn person_name(person: workspace.Person) -> Text {
    if person.you {
        "You"
    } else if person.name != "" { person.name } else { short_id(person.person) }
}

/// The first eight characters of a person id, so a row names who it is without the whole key.
fn short_id(id: Text) -> Text {
    if id.len() > 8 { "{id.slice(0, 8)}…" } else { id }
}

/// A person's part, in a line: their role, and the workspace being theirs where it is.
fn person_words(person: workspace.Person) -> Text {
    if person.owner {
        "{person.role_name} · the workspace is theirs"
    } else if person.administrator {
        "{person.role_name} · administrator"
    } else { person.role_name }
}

/// What an agent is, in a line: the model it plans with, or the workspace's own.
fn agent_words(agent: workspace.Agent) -> Text {
    if let model = agent.model {
        "{agent_state_words(agent.state)} · {model}"
    } else {
        "{agent_state_words(agent.state)} · this workspace's model"
    }
}

/// How an agent stands, in the person's words.
fn agent_state_words(state: workspace.AgentState) -> Text {
    if state == .active { "Active" } else if state == .paused { "Paused" } else { "Retired" }
}

/// The workspace in front's grants: what it grants beyond its roles and its own documents. Every
/// workspace's when no workspace is open here.
fn permissions_of(access: workspace.Access) -> List<workspace.AccessEntry> {
    let workspace_id = if let gov = access.governance { gov.workspace } else { "" }
    access.workspaces
        .filter(w -> workspace_id == "" or w.id == workspace_id)
        .flat_map(w -> w.grants)
}
