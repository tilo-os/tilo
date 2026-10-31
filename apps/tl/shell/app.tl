app so.tilo.shell "Shell" (icon: .monitor, category: .system)

/// This device's desk as the session holds it: the time in the person's style, who they are,
/// their notifications, the workspaces on this device and the tasks running for them. The
/// session keeps it in step; the shell's views read it.
type Desk (origin: session.desk) {
    person: Text (title)
    clock: Text
    weekday: Text
    date: Text
    unread: Int
    quiet: Bool
    status_panel: Bool
    workspace: Text
    strip: Text
    workspaces: Collection<Workspace>
    tasks: Collection<Task>
    people: Collection<Person>
}

/// A workspace on this device.
type Workspace (origin: session.workspace) {
    key: Text merge register
    name: Text (title)
    current: Bool
}

/// A task running for the person: an agent's or a script's.
type Task (origin: session.task) {
    key: Text merge register
    title: Text (title)
    state: Text
}

/// Someone else here in the workspace in front.
type Person (origin: session.person) {
    key: Text merge register
    name: Text (title)
}

root Desk

default view bar for Desk

/// Brings the workspace to the front.
action switch_to(w: Workspace) (icon: .layers, label: "Switch to") {
    require not w.current "It is in front already"
    outside("session.switch_workspace", w.key)
}

/// Opens or closes the system's menu: about Tilo, settings, sleep and power.
action system_menu(d: Desk) (icon: .tilo, label: "Tilo") {
    outside("session.act", "toggle-tilo-menu")
}

/// Shows the strips of the workspace in front, to choose one.
action strips(d: Desk) (icon: .chevron_right, label: "Strips") {
    outside("session.act", "show-strips")
}

/// Opens or closes the device's status: quick settings, or the notifications.
action status(d: Desk) (icon: .bell, label: "Notifications") {
    outside("session.act", "toggle-quick-settings")
}

/// Makes a new workspace on this device and brings it to the front.
action new_workspace(d: Desk) (icon: .plus, label: "New workspace") {
    outside("session.act", "new-workspace")
}

/// Shows the workspaces this device may join.
action join_workspace(d: Desk) (icon: .users, label: "Join a workspace") {
    outside("session.act", "join-workspace")
}

/// Opens the settings of the workspace in front.
action workspace_settings(d: Desk) (icon: .settings, label: "Workspace settings") {
    outside("session.act", "workspace-settings")
}

/// Shows every workspace's strips and windows at once.
action overview(d: Desk) (icon: .layout_grid, label: "Overview") {
    outside("session.act", "toggle-overview")
}

/// The top bar: the system's menu and the workspace in front, which opens the workspaces to
/// switch between, then the tasks running; the time in the middle; the people here, the person
/// and the device's status at the end. Each part is a view of its own, so a change runs only
/// the part that reads it: the minute, the clock alone. Narrower than its parts, the bar takes a
/// second line rather than pressing one part into another.
view bar(d: Desk) {
    row(padding: .tight, align: .center, wrap: true) {
        places(d)
        spacer
        clock(d)
        spacer
        presence(d)
    }
}

/// Where the person is: the system's menu, the workspace in front with the workspaces to
/// switch to, the strip in front, and the tasks running. A narrow output keeps the menu and the
/// workspace; the strip and the tasks are in the overview.
view places(d: Desk) {
    row(gap: .tight, align: .center) {
        ui.Button("Tilo", icon: .tilo, icon_only: true, role: .bar) {
            on press: system_menu(d)
        }
        ui.MenuButton(d.workspace, role: .bar) {
            for w in d.workspaces {
                ui.MenuItem(w.name, checked: .some(w.current)) {
                    on choose: switch_to(w)
                }
            }
            ui.MenuItem("New workspace", icon: .plus) {
                on choose: new_workspace(d)
            }
            ui.MenuItem("Join a workspace", icon: .users) {
                on choose: join_workspace(d)
            }
            ui.MenuItem("Workspace settings", icon: .settings) {
                on choose: workspace_settings(d)
            }
            ui.MenuItem("Overview", icon: .layout_grid) {
                on choose: overview(d)
            }
        }
        if not space.compact {
            if d.strip != "" {
                ui.Button(d.strip, icon: .chevron_right, role: .bar) {
                    on press: strips(d)
                }
            }
            for t in d.tasks {
                ui.Button(t.title, icon: .bot, role: .bar)
            }
        }
    }
}

/// The time, to the minute.
view clock(d: Desk) {
    ui.Text(d.clock, role: .subheading, numeric: true)
}

/// Who is here, the person, and the device's status. A narrow output keeps the status; who is
/// here is in the system's menu.
view presence(d: Desk) {
    row(gap: .sibling, align: .center, distribute: .end) {
        if not space.compact {
            for p in d.people {
                ui.Avatar(name: .some(p.name))
            }
            ui.Avatar(name: .some(d.person))
        }
        ui.Button(if d.unread > 0 { "{d.unread}" } else { "Notifications" }, icon: if d.quiet { .bell_off } else { .bell }, icon_only: d.unread == 0, role: .bar) {
            on press: status(d)
        }
    }
}

/// An afternoon at the desk: two workspaces, the one in front on its first strip, someone else
/// here, a task running and two notifications unread.
scenario "an afternoon at the desk" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "4:28 PM"
    d.weekday = "Thursday"
    d.date = "October 30"
    d.unread = 2
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
    d.tasks.insert(Task(key: "sort-mail", title: "Sorting the inbox", state: "running"))
}
