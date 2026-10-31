app so.tilo.shell "Shell" (icon: .monitor, category: .system)

/// This device's desk as the session holds it: the time in the person's style, who they are,
/// their notifications, the workspaces on this device and the tasks running for them. The
/// session keeps it in step; the shell's views read it. Its text is the session's to replace
/// whole, never edited here: each is a register, not text to merge.
type Desk (origin: session.desk) {
    person: Text merge register (title)
    clock: Text merge register
    weekday: Text merge register
    date: Text merge register
    unread: Int
    quiet: Bool
    status_panel: Bool
    workspace: Text merge register
    strip: Text merge register
    workspaces: Collection<Workspace>
    tasks: Collection<Task>
    people: Collection<Person>
}

/// A workspace on this device.
type Workspace (origin: session.workspace) {
    key: Text merge register
    name: Text merge register (title)
    current: Bool
}

/// A task running for the person: an agent's or a script's.
type Task (origin: session.task) {
    key: Text merge register
    title: Text merge register (title)
    state: Text merge register
}

/// Someone else here in the workspace in front.
type Person (origin: session.person) {
    key: Text merge register
    name: Text merge register (title)
}

root Desk

default view chrome for Desk

default view shell_settings for Shell

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

/// The shell on this device: the bar and the dock on the edges the person's settings name.
/// Which parts the bar has, what the dock holds and when each shows come from their `Shell`
/// record, so this view is the whole of what a preset changes. Nothing is drawn between the
/// two: the windows stand there, the compositor's under this view.
view chrome(d: Desk) {
    let s = shell
    layers {
        if bar_shows(s) {
            bar_on_edge(s, d)
        }
        if dock_shows(s) {
            if s.dock_edge == .bottom {
                row(at: .bottom, fill, align: .center, padding: .related) {
                    dock(s, d)
                }
            } else if s.dock_edge == .top {
                row(at: .top, fill, align: .center, padding: .related) {
                    dock(s, d)
                }
            } else if s.dock_edge == .left {
                row(at: .leading, align: .center, padding: .related) {
                    dock(s, d)
                }
            } else {
                row(at: .trailing, align: .center, padding: .related) {
                    dock(s, d)
                }
            }
        }
    }
}

/// The shell's whole desktop, as a person sees it when they open the shell: the chrome, and
/// between it the place the windows stand in — the workspace and strip in front, the time, and
/// who is here, the desk's own reading of where the person is.
view desktop(d: Desk) {
    layers {
        row(fill, align: .center) {
            stack(gap: .section, align: .center) {
                ui.Text(d.workspace, role: .caption, secondary: true)
                ui.Text(d.clock, role: .display)
                ui.Text(d.date, role: .caption, secondary: true)
            }
            presence(d)
        }
        chrome(d)
    }
}

/// The top bar on the edge it rides: the whole width with its parts spread along it at the
/// `full` fit, an inset run centred on the edge at the `floating` one — where it covers less of
/// what is under it. Its ground is the theme's bar material either way; how much of the output
/// it spans is what its fit says.
view bar_on_edge(s: Shell, d: Desk) {
    let on_top = s.bar_edge == .top
    if s.bar_fit == .full {
        if on_top {
            row(at: .top, fill, align: .center, padding: .tight) {
                top_bar(d)
            }
        } else {
            row(at: .bottom, fill, align: .center, padding: .tight) {
                top_bar(d)
            }
        }
    } else if on_top {
        row(at: .top, align: .center, padding: .related) {
            top_bar(d)
        }
    } else {
        row(at: .bottom, align: .center, padding: .related) {
            top_bar(d)
        }
    }
}

/// The top bar: the parts the person put on it, in their order, each a view of its own — so
/// a change runs only the part that reads it: the minute, the clock alone. The bar is a run
/// the person reorders by dragging a part, and a part dragged onto it from the settings or
/// the dock's list lands where it is dropped; letting go runs one action, `place_part`.
view top_bar(d: Desk) {
    let s = shell
    ui.DragRun(axis: .horizontal, name: "Top bar") {
        on drop(moved: BarPart, at: Int): place_part(s, moved, at)
        for part in s.bar_parts {
            if part.shown {
                ui.DragItem(part, title: part.title) {
                    bar_part(d, part)
                }
            }
        }
    }
}

/// One part of the bar, drawn from what it is. A narrow output keeps the menu, the
/// workspace and the status; the strip, the tasks and who is here give way, because the bar's
/// parts must never press into each other (the overview shows them).
view bar_part(d: Desk, part: BarPart) {
    if part.slot == .space {
        spacer
    } else if part.slot == .clock {
        clock(d)
    } else if part.slot == .menu {
        ui.Button("Tilo", icon: .tilo, icon_only: true, role: .bar) {
            on press: system_menu(d)
        }
    } else if part.slot == .workspace {
        workspace_menu(d)
    } else if part.slot == .app_menu {
        ui.MenuButton("File", role: .bar) {
            ui.MenuItem("Close window", icon: .x)
            ui.MenuItem("Save", icon: .download)
        }
    } else if part.slot == .strip {
        if d.strip != "" and not space.compact {
            ui.Button(d.strip, icon: .chevron_right, role: .bar) {
                on press: strips(d)
            }
        }
    } else if part.slot == .tasks {
        if not space.compact {
            for t in d.tasks {
                ui.Button(t.title, icon: .bot, role: .bar)
            }
        }
    } else if part.slot == .people {
        if not space.compact {
            for p in d.people {
                ui.Avatar(name: .some(p.name))
            }
            ui.Avatar(name: .some(d.person))
        }
    } else if part.slot == .power {
        ui.Button("Power", icon: .battery, icon_only: true, role: .bar)
    } else if part.slot == .agent {
        for t in d.tasks {
            ui.Icon(.sparkles, size: .small, tone: .info)
        }
    } else if part.slot == .status {
        bell(d)
    }
}

/// The workspace in front, opening the workspaces to switch between and the actions on this
/// device's workspaces.
view workspace_menu(d: Desk) {
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
        bell(d)
    }
}

/// The device's status: an icon button named with what it shows — "Notifications", how many
/// are unread, or that they are silenced — with the count above it as a badge, so the name
/// assistive technology reads is the whole fact and the number is for the eye.
view bell(d: Desk) {
    let name = if d.quiet { "Notifications silenced" } else { "Notifications" }
    let label = if d.unread == 0 { name } else if d.unread == 1 { "{name}, 1 unread" } else { "{name}, {d.unread} unread" }
    stack(gap: .tight, align: .center) {
        ui.Button(label, icon: if d.quiet { .bell_off } else { .bell }, icon_only: true, role: .bar) {
            on press: status(d)
        }
        if d.unread > 0 {
            ui.Badge(count: .some(d.unread))
        }
    }
}

/// An afternoon at the desk: two workspaces, the one in front on its first strip, someone else
/// here, a task running and two notifications unread, and the default shell — the bar on the
/// top and the dock on the bottom.
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true, badge: 2, attention: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    s.dock_items.insert(DockItem(key: "controls", title: "Quick settings", icon: "settings-2"))
}

/// The desktop as a person sees it with the default shell: the bar across the top, the dock
/// on the bottom, and the desk between them.
scenario "the default shell on a desk" {
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true, badge: 2, attention: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
}

/// Thirty items on the dock: the person's apps on a working machine, three of them running,
/// one wanting them.
scenario "a full dock" {
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "a01", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "a02", title: "Mail", icon: "mail", badge: 12))
    s.dock_items.insert(DockItem(key: "a03", title: "Chat", icon: "message-square", running: true, attention: true))
    s.dock_items.insert(DockItem(key: "a04", title: "Calendar", icon: "calendar"))
    s.dock_items.insert(DockItem(key: "a05", title: "Tasks", icon: "list-checks", badge: 4))
    s.dock_items.insert(DockItem(key: "a06", title: "Files", icon: "folder"))
    s.dock_items.insert(DockItem(key: "a07", title: "Music", icon: "music"))
    s.dock_items.insert(DockItem(key: "a08", title: "Photos", icon: "image"))
    s.dock_items.insert(DockItem(key: "a09", title: "Maps", icon: "map-pin"))
    s.dock_items.insert(DockItem(key: "a10", title: "Weather", icon: "cloud"))
    s.dock_items.insert(DockItem(key: "a11", title: "Terminal", icon: "terminal"))
    s.dock_items.insert(DockItem(key: "a12", title: "Settings", icon: "settings"))
    s.dock_items.insert(DockItem(key: "a13", title: "Camera", icon: "camera"))
    s.dock_items.insert(DockItem(key: "a14", title: "Contacts", icon: "users"))
    s.dock_items.insert(DockItem(key: "a15", title: "Clocks", icon: "clock"))
    s.dock_items.insert(DockItem(key: "a16", title: "Calculator", icon: "hash"))
    s.dock_items.insert(DockItem(key: "a17", title: "Canvas", icon: "pen-tool"))
    s.dock_items.insert(DockItem(key: "a18", title: "Sheets", icon: "table"))
    s.dock_items.insert(DockItem(key: "a19", title: "Slides", icon: "monitor"))
    s.dock_items.insert(DockItem(key: "a20", title: "Docs", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "a21", title: "Reader", icon: "bookmark"))
    s.dock_items.insert(DockItem(key: "a22", title: "Feeds", icon: "rss"))
    s.dock_items.insert(DockItem(key: "a23", title: "Passwords", icon: "fingerprint"))
    s.dock_items.insert(DockItem(key: "a24", title: "Shortcuts", icon: "command"))
    s.dock_items.insert(DockItem(key: "a25", title: "Store", icon: "shopping-bag"))
    s.dock_items.insert(DockItem(key: "a26", title: "Print", icon: "printer"))
    s.dock_items.insert(DockItem(key: "a27", title: "Disks", icon: "hard-drive"))
    s.dock_items.insert(DockItem(key: "a28", title: "Monitor", icon: "screen-share"))
    s.dock_items.insert(DockItem(key: "a29", title: "Speed test", icon: "gauge"))
    s.dock_items.insert(DockItem(key: "a30", title: "Video", icon: "play"))
}

/// The dock on the leading edge, as a person who works in a wide column sets it.
scenario "the dock on the left" {
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
    seed(shell)
    let s = shell
    s.dock_edge = .left
    s.dock_fit = .floating
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
}

/// The dock on the trailing edge, full width with the bar off: a Windows-like taskbar on a
/// person's second screen.
scenario "the dock on the right" {
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
    seed(shell)
    let s = shell
    s.dock_edge = .right
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
}

/// A dock item mid-drag in the edit mode, with the place it would land marked: the person is
/// moving Chat before Mail.
scenario "moving an item on the dock" {
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
}

/// The bar with its parts rearranged: the clock first, then the menu, and the tasks after the
/// status.
scenario "a rearranged bar" {
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
    seed(shell)
    let s = shell
    if let clock = s.bar_parts.find(p -> p.slot == Slot.clock) { s.bar_parts.move(clock, to: 0) }
    if let tasks = s.bar_parts.find(p -> p.slot == Slot.tasks), status = s.bar_parts.find(p -> p.slot == Slot.status) { s.bar_parts.move(tasks, after: status) }
    s.tuned = true
    s.based_on = .tilo
}

/// The desktop as each preset lays the person's shell out: the bar and the dock on the edges
/// the preset gives them, shown the way it says. A person who picks one sees this, and a shot
/// of it is what the layout check keeps per theme.
scenario "the Tilo preset" {
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    apply(s, .tilo)
}

scenario "the macos preset" {
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    apply(s, .macos)
}

scenario "the windows preset" {
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
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    apply(s, .windows)
}

scenario "the gnome preset" {
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
    seed(shell)
    apply(shell, .gnome)
}

scenario "the minimal preset" {
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
    seed(shell)
    apply(shell, .minimal)
}
