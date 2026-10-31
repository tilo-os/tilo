app so.tilo.shell "Shell" (icon: .monitor, category: .system)

/// This device's desk as the session holds it: the time in the person's style, who they are,
/// their notifications, the workspaces on this device and the tasks running for them. The
/// session keeps it in step; the shell's views read it.
type Desk (origin: session.desk) {
    person: Text (title)
    clock: Text
    /// The timer or stopwatch Clocks runs, by its name: its label, or `Timer` or `Stopwatch`
    /// when the app named none; empty when none floats.
    timer: Text
    /// What the timer reads: what it has left, or what the stopwatch has run.
    timer_left: Text
    timer_paused: Bool
    timer_stopwatch: Bool
    /// The question the session puts to the person about themselves; empty when none waits.
    ask: Text
    /// What answering the question brings.
    ask_detail: Text
    /// The question's answer, as its button says it.
    ask_answer: Text
    /// Whether the question stands open on the person's avatar.
    ask_open: Bool
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
    /// The running applications' tray items, in the order they registered.
    tray: Collection<TrayItem>
}

/// A workspace on this device.
type Workspace (origin: session.workspace) {
    key: Text merge register
    name: Text (title)
    current: Bool
}

/// A running application's tray item: what the bar shows, and what a press on it reaches.
type TrayItem (origin: session.tray_item) {
    key: Text merge register
    title: Text merge register (title)
    menu: Bool
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

/// Opens or closes a running application's tray item: its window, or its menu.
///
/// The item's key names it; the session reads the same tray, so the action takes the key and
/// the bar's item is the one it means. Each item's action is its own, so a press names which.
action open_tray_item(d: Desk, item: TrayItem) (icon: .app_window, label: "Open") {
    outside("session.tray", item.key)
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

/// Answers the question waiting on the person's avatar.
action answer_ask(d: Desk) (icon: .check, label: "Answer") {
    outside("session.act", "answer_ask")
}

/// Puts the question on the person's avatar off for now.
action put_off_ask(d: Desk) (icon: .x, label: "Ask me later") {
    outside("session.act", "put_off_ask")
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

/// The time, to the minute, and the timer Clocks runs beside it: its label and what it has
/// left (a stopwatch: what it has run), paused when the person paused it.
view clock(d: Desk) {
    row(gap: .tight, align: .center) {
        ui.Text(d.clock, role: .subheading, numeric: true)
        if d.timer != "" {
            // The name is a text of its own, so a reader hears it; the reading is the number
            // beside it, and a paused timer says so.
            ui.Text(d.timer, role: .caption)
            ui.Text(if d.timer_paused { "Paused · {d.timer_left}" } else { d.timer_left }, role: .caption, numeric: true)
        }
    }
}

/// Who is here, the person, and the device's status. A narrow output keeps the status; who is
/// here is in the system's menu.
view presence(d: Desk) {
    row(gap: .sibling, align: .center, distribute: .end) {
        if not space.compact {
            for p in d.people {
                ui.Avatar(name: .some(p.name))
            }
            // The person's own avatar carries the question put to them: their answer or their
            // putting it off, never a banner over their windows.
            if d.ask != "" {
                ui.Button(d.ask, role: .bar) {
                    on press: answer_ask(d)
                }
                if d.ask_answer != "" {
                    ui.Button(d.ask_answer, role: .bar) {
                        on press: answer_ask(d)
                    }
                }
                ui.Button("Ask me later", role: .bar) {
                    on press: put_off_ask(d)
                }
            } else {
                ui.Avatar(name: .some(d.person))
            }
        }
        // The running applications' tray items, each under the app's own words for it.
        for item in d.tray {
            ui.Button(item.title, role: .bar) {
                on press: open_tray_item(d, item)
            }
        }
        // The bell is named with what it shows; the count beside it is for the eye.
        let bell = if d.quiet { "Notifications silenced" } else { "Notifications" }
        ui.Button(if d.unread == 0 { bell } else if d.unread == 1 { "{bell}, 1 unread" } else { "{bell}, {d.unread} unread" }, icon: if d.quiet { .bell_off } else { .bell }, icon_only: true, role: .bar) {
            on press: status(d)
        }
        if d.unread > 0 {
            ui.Badge(count: .some(d.unread))
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
