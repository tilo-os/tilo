/// The shell's chrome as each preset lays it out: the bar on the edge and fit the preset gives
/// it, the dock on its own, shown the way it says. A person who picks one sees this, and a shot
/// of each is what the layout check keeps per theme: the bar's parts still line up (the run
/// spreads them, never pressing them into the left edge) and the dock's items sit on their
/// edge, the Windows preset's taskbar carrying the time and the status at its end.
///
/// They never touch a person's own record: a real first launch starts empty.
scenario "the default shell on a desk" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "4:28 PM"
    d.weekday = "Thursday"
    d.date = "October 30"
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(
        DockItem(
            key: "chat",
            title: "Chat",
            icon: "message-square",
            running: true,
            badge: 2,
            attention: true,
        ),
    )
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "a01", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "a02", title: "Mail", icon: "mail", badge: 12))
    s.dock_items.insert(
        DockItem(key: "a03", title: "Chat", icon: "message-square", running: true, attention: true),
    )
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
    seed(shell)
    let s = shell
    if let clock = s.bar_parts.find(p -> p.slot == Slot.clock) { s.bar_parts.move(clock, to: 0) }
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
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
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
    seed(shell)
    apply(shell, .gnome)
}

scenario "the minimal preset" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "4:28 PM"
    d.weekday = "Thursday"
    d.date = "October 30"
    seed_waiting(d)
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia"))
    seed(shell)
    apply(shell, .minimal)
}
