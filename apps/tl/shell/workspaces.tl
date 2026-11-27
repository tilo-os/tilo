/// The workspace overview: every workspace the person is in, to switch to, join, or make one,
/// and below them the person's Tilo as a whole — their workspaces elsewhere, the hosts running
/// theirs and the agents they let in. It is not the window overview (that is the layout's windows
/// and strips); a workspace is where a person and their team work, and a window is where something
/// shows.
///
/// Every fact is read from the one place that publishes it: the workspaces from the layout
/// (`session.layout`), the hosts and the workspaces elsewhere from the device (`device.yours`),
/// and the agents from the person's keys (`device.devices`). Every action is one the session, the
/// host or the device already offers: switching to a workspace, going to one elsewhere, managing
/// a plan on the provider's page, and removing a key.
view workspaces() {
    state name: Text = ""
    // The cards stand in the shell's page, as the overview's do: their corners round, and a card
    // on the window's edge has them cut by it.
    page(full) {
        stack(gap: .section) {
            ui.Text("Workspaces", role: .title)
            grid(min: 18rem) {
                for w in layout.workspaces.sort_by(w -> w.name) {
                    workspace_card(w)
                }
            }
            row(gap: .related, align: .end, wrap: true) {
                ui.TextInput(bind: name, placeholder: "Name a new workspace", fill) {
                    on submit: session.new_workspace(name: name).then(set(name, ""))
                }
                ui.Button("Create", icon: .plus, role: .primary) {
                    on press: session.new_workspace(name: name)
                }
                ui.Button("Join a workspace", icon: .users) {
                    on press: session.join_workspace()
                }
            }
            your_tilo()
        }
    }
}

/// One workspace: its name, and for the one in front, who is here. The one in front is lifted
/// and takes no press, as a card that says where the person already is; the others switch to it.
/// Where the shell is a page and the workspace opens as one of its own (`LayoutWorkspace.home`),
/// its card opens that page, as the workspace switcher has always led from one workspace to
/// another.
view workspace_card(w: LayoutWorkspace) {
    if w.home != "" {
        ui.Card(title: w.name, selected: w.current) {
            ui.Link(w.home, label: "Open")
        }
    } else if w.current {
        // The workspace in front takes no press: the card says where the person already is.
        ui.Card(item: w, title: w.name, selected: true) {
            if not layout.people.is_empty() {
                ui.Facts {
                    ui.Fact("{layout.people.len()} here", icon: .users)
                }
            }
        }
    } else {
        ui.Card(item: w, title: w.name, selected: false) {
            on activate: session.switch_workspace(workspace: w.key)
        }
    }
}

/// The person's Tilo below their workspaces: their workspaces elsewhere, the hosts running theirs
/// with the plan, what it used and its credit, and the agents they let in. It holds up with none
/// of them — each section says what that means — and every read it draws has a state of its own,
/// so a host that was not reached never looks like an account that never existed.
view your_tilo() {
    // Read once, so a section that hides itself still reads the same answer as the one beside it.
    let yours = device.yours
    ui.Text("Your Tilo", role: .heading)
    if not yours.elsewhere.is_empty() {
        ui.Section("More workspaces") {
            for e in yours.elsewhere {
                ui.ListRow(e.name, secondary: "On one of your hosts", icon: .layers) {
                    on activate: device.host_action(device.people, id: e.action)
                }
            }
        }
    }
    hosts_section(yours.hosts)
    agents_section()
}

/// The hosts running the person's workspaces: each its name and what the account is on, the
/// provider's page where a card or a plan changes, and, when none runs their workspaces elsewhere,
/// the fact that this computer runs them itself.
view hosts_section(hosts: List<device.Host>) {
    ui.Section("Hosts & compute") {
        if hosts.is_empty() {
            ui.EmptyState(
                "No other machine runs your workspaces",
                message: "This computer runs them itself, and there is nothing to pay for.",
                icon: .monitor,
                inline: true,
            )
        }
        for h in hosts {
            ui.ListRow(
                h.server,
                secondary: host_words(h),
                icon: if h.kind == "provider" {
                    .cloud
                } else if h.kind == "self-hosted" { .hard_drive } else { .monitor },
            ) {
                trailing: {
                    if h.manage != "" {
                        ui.Button(
                            if h.has_card { "Manage billing" } else { "Add a card" },
                            icon: .external_link,
                            size: .small,
                        ) {
                            on press: open_page(h.manage)
                        }
                    }
                }
            }
        }
    }
}

/// The agents the person let in, drawn from the keys that act as them: each its name, what it is,
/// and the way to take its access back. A key already removed says so and offers nothing.
view agents_section() {
    let agents = device.devices.access.filter(
        one -> one.kind == .agent or one.agent != "" or one.kind == .script,
    )
    ui.Section("Agents let in") {
        if agents.is_empty() {
            ui.EmptyState(
                "No agent acts as you yet",
                message: "An agent you let in shows here, with the way to remove it.",
                icon: .bot,
                inline: true,
            )
        }
        for one in agents {
            ui.ListRow(one.label, secondary: agent_words(one), icon: .bot) {
                trailing: {
                    if not one.revoked {
                        agent_menu(one)
                    }
                }
            }
        }
    }
}

/// What a host's row says under its name: what the account is on and what it used, or why it says
/// nothing of either. A host that did not answer, or that nobody signed in to, is said in the
/// host's or this device's own words rather than as an account with no plan.
fn host_words(h: device.Host) -> Text {
    if h.state == "reading" {
        std.say("Reading your account…")
    } else if h.state == "unreachable" {
        if h.words != "" {
            h.words
        } else {
            std.say("Your host could not be reached.")
        }
    } else if h.state == "signed-out" {
        if h.words != "" {
            h.words
        } else {
            std.say("Not signed in yet, so there is no plan to show.")
        }
    } else if h.state == "no-account" {
        if h.words != "" {
            h.words
        } else {
            std.say("Your workspaces run on this machine itself, so there is nothing to pay for.")
        }
    } else {
        let plan = if h.plan_name == "" {
            std.say("No plan yet")
        } else if h.plan_price == "" {
            h.plan_name
        } else { std.say("{h.plan_name}, {h.plan_price}") }
        let used = if let included = h.included_seconds {
            std.say("{machine_time(h.usage_seconds)} of {machine_time(included)} this period")
        } else if h.usage_seconds == 0 {
            std.say("Nothing used this period")
        } else {
            std.say("{machine_time(h.usage_seconds)} this period")
        }
        if h.credit == "" {
            std.say("{plan} · {used}")
        } else {
            std.say("{plan} · {used} · {h.credit} credit")
        }
    }
}

/// What an agent's row says under its name: whether it still acts as the person, and whether it
/// reaches this device.
fn agent_words(one: device.AccessKey) -> Text {
    if one.revoked {
        std.say("Removed; every device of yours refuses it")
    } else if one.kind == .script {
        std.say("A program of yours, acting as you")
    } else if one.online { std.say("Online now, acting as you") } else { std.say("Acting as you") }
}

/// The ways to take an agent's access back, each its reason: what cannot be undone is asked for
/// here, on the row, with the reason in the words the person chooses.
view agent_menu(one: device.AccessKey) {
    ui.MenuButton("Manage {one.label}", icon: .ellipsis) {
        ui.MenuItem("Remove {one.label} — no longer used", destructive: true, icon: .trash) {
            on choose: device.revoke(device.devices, device: one.id, reason: "retired")
        }
        ui.MenuItem("Remove {one.label} — it was lost", destructive: true, icon: .trash) {
            on choose: device.revoke(device.devices, device: one.id, reason: "lost")
        }
        ui.MenuItem("Remove {one.label} — it was stolen", destructive: true, icon: .trash) {
            on choose: device.revoke(device.devices, device: one.id, reason: "stolen")
        }
        ui.MenuItem(
            "Remove {one.label} — someone knows its key",
            destructive: true,
            icon: .trash,
        ) {
            on choose: device.revoke(device.devices, device: one.id, reason: "compromised")
        }
    }
}

/// A length of machine time, as a person reads it: hours and minutes, minutes, or seconds.
fn machine_time(seconds: Int) -> Text {
    let hours = seconds / 3600
    let minutes = seconds % 3600 / 60
    if hours > 0 {
        if minutes > 0 { std.say("{hours} hr {minutes} min") } else { std.say("{hours} hr") }
    } else if minutes > 0 { std.say("{minutes} min") } else { std.say("{seconds} s") }
}

/// Open a web page in the app that opens web addresses. The provider's own billing page is the
/// only one this page passes, so nothing a host answers with can open anything else.
action open_page(url: Text) (label: "Open a web page") {
    match tool.open_link(url) {
        .ok => {}
        .err(_) => {}
    }
}
