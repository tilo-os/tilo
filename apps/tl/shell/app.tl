app so.tilo.shell "Shell" (icon: .monitor, category: .system, records: .own)

use so.tilo.lang.chat as chat
use so.tilo.lang.notes as notes
use so.tilo.lang.tasks as tasks

/// This device's desk as the session holds it: the time in the person's style, who they are,
/// their notifications, the workspaces on this device, the tasks running for them and the
/// surface the desk shows over its windows. The session keeps it in step; the shell's views
/// read it. Its text is the session's to replace whole, never edited here: each is a register,
/// not text to merge.
type Desk (origin: session.desk) {
    person: Text merge register (title)
    /// The surface the person is in: `none` at rest, else one of `surface`'s names.
    here: Text merge register
    /// What the person typed in the shell's search, which opens the launcher where they left off.
    spotlight: Text merge register
    /// The notice shown as a banner over the desk now, by its id.
    banner: Text merge register
    clock: Text merge register
    /// The timer or stopwatch Clocks runs, by its name: its label, or `Timer` or `Stopwatch`
    /// when the app named none; empty when none floats.
    timer: Text merge register
    /// What the timer reads: what it has left, or what the stopwatch has run.
    timer_left: Text merge register
    timer_paused: Bool
    timer_stopwatch: Bool
    /// The question the session puts to the person about themselves; empty when none waits.
    ask: Text merge register
    /// What answering the question brings.
    ask_detail: Text merge register
    /// The question's answer, as its button says it.
    ask_answer: Text merge register
    /// Whether the question stands open on the person's avatar.
    ask_open: Bool
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
    /// The pointer at the top edge, and a window standing where the bar rides: what a bar that
    /// shows only sometimes follows. The session writes them with the rest of the desk; no
    /// action changes them and no preset names them.
    bar_reached: Bool
    bar_covered: Bool
    /// The pointer at the bottom edge, and a window standing where the dock rides.
    dock_reached: Bool
    dock_covered: Bool
    agents: Collection<Agent>
    rooms: Collection<Room>
    notices: Collection<Notice>
    notice_actions: Collection<NoticeAction>
    windows: Collection<Win>
    /// The running applications' tray items, in the order they registered.
    tray: Collection<TrayItem>
}

/// A workspace on this device.
type Workspace (origin: session.workspace) {
    key: Text merge register
    name: Text merge register (title)
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
    title: Text merge register (title)
    state: Text merge register
}

/// Someone else here in the workspace in front.
type Person (origin: session.person) {
    key: Text merge register
    name: Text merge register (title)
}

/// A window of the desk: a tab of a strip in the workspace in front.
type Win (origin: session.window) {
    key: Text merge register
    title: Text merge register (title)
    origin_app: Text merge register
    icon: Text merge register
    strip: Text merge register
    workspace: Text merge register
    recency: Int
    focused: Bool
    preview: Text merge register
}

/// An agent here for the person, or that acted for them.
type Agent (origin: session.agent) {
    key: Text merge register
    name: Text merge register (title)
    state: Text merge register
    doing: Text merge register
    mark: Text merge register
    unseen: Int
}

/// A workspace the person is in, as the workspace overview names it.
type Room (origin: session.room) {
    key: Text merge register
    name: Text merge register (title)
    current: Bool
    members: Text merge register
    present: Int
    activity: Text merge register
}

/// A notification in the centre, as its sender or its task named it.
type Notice (origin: session.notice) {
    key: Text merge register
    title: Text merge register (title)
    body: Text merge register
    origin_app: Text merge register
    icon: Text merge register
    kind: Text merge register
    read: Bool
    group: Text merge register
    wants_reply: Bool
}

/// One action a notification offers, as its sender named it.
type NoticeAction (origin: session.notice_action) {
    notice: Text merge register
    key: Text merge register
    label: Text merge register (title)
}

root arrangement: Arrangement

root Desk

default view desk for Desk

default view desk_view for arrangement

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

/// Shows the windows and strips of every workspace at once, to search and move things.
action show_overview(d: Desk) (icon: .layout_grid, label: "Overview") {
    outside("session.act", "toggle-overview")
}

/// Shows the windows of the workspace, most recent first, as Alt+Tab walks them.
action switch_windows(d: Desk) (icon: .layers, label: "Switch windows") {
    outside("session.act", "toggle-switcher")
}

/// Answers the question waiting on the person's avatar.
action answer_ask(d: Desk) (icon: .check, label: "Answer") {
    outside("session.act", "answer_ask")
}

/// Puts the question on the person's avatar off for now.
action put_off_ask(d: Desk) (icon: .x, label: "Ask me later") {
    outside("session.act", "put_off_ask")
}

/// Opens search and launching over the whole workspace: apps, resources, actions and recents.
action open_launcher(d: Desk) (icon: .search, label: "Search") {
    outside("session.act", "toggle-launcher")
}

/// Shows the notifications in the centre.
action open_centre(d: Desk) (icon: .bell, label: "Notifications") {
    outside("session.act", "show-notifications")
}

/// Opens the person's other workspaces: who is in each, what it holds, switching, joining and
/// making one.
action open_rooms(d: Desk) (icon: .layers, label: "Workspaces") {
    outside("session.act", "toggle-workspace-menu")
}

/// Asks the agent last asked, or the first live one, about something: the composer, over what
/// the person pointed at or what they selected.
action ask(d: Desk) (icon: .bot, label: "Ask an agent") {
    outside("session.act", "toggle-composer")
}

/// The person's desk as a window shows it: the top bar over the preset they picked, and the
/// preset switch beside the bar's actions — or, for the traditional desktop, the whole screen:
/// its frames on the desk under the desk's own bar and dock, as the session shows them on an
/// output. The bar reads `desk`, this package's other root, as
/// any view reads a root of the package it uses. A change of the arrangement runs the preset
/// body; a change of the desk reading runs the bar.
view desk_view(a: Arrangement) {
    let v = viewing
    if v.showing == .desktop {
        layers(fill) {
            traditional_desktop(a, fill)
            desk(root Desk, fill)
        }
    } else {
        ui.NavShell(content_kind: .page) {
            actions: {
                bar(root Desk)
                preset_switch(v)
            }
            content: {
                if v.showing == .frames {
                    framed_canvas(a)
                } else if v.showing == .spaces {
                    spaces_canvas(a)
                } else {
                    strips_view(a, v)
                }
            }
        }
    }
}

/// The time, to the minute, and the timer Clocks runs beside it: its label and what it has
/// left (a stopwatch: what it has run), paused when the person paused it. The name is a text of
/// its own, so a reader hears it; the reading is the number beside it, and a paused timer says
/// so.
view clock(d: Desk) {
    row(gap: .tight, align: .center) {
        ui.Text(d.clock, role: .subheading, numeric: true)
        if d.timer != "" {
            ui.Text(d.timer, role: .caption)
            ui.Text(if d.timer_paused { "Paused · {d.timer_left}" } else { d.timer_left }, role: .caption, numeric: true)
        }
    }
}

/// The top bar: the parts the person put on it, in their order, each a view of its own — so a
/// change runs only the part that reads it: the minute, the clock alone. The bar is a run the
/// person reorders by dragging a part, and a part dragged onto it from the settings lands where
/// it is dropped; letting go runs one action, `place_part`.
view bar(d: Desk) {
    let s = shell
    let roomy = space.width_class == .expanded or space.width_class == .wide
    ui.DragRun(axis: .horizontal, name: "Top bar") {
        on drop(moved: BarPart, at: Int): place_part(s, moved, at)
        for part in s.bar_parts {
            if part.shown and (space.wide or not part_gives_way(part, roomy)) {
                ui.DragItem(part, title: part.title, group: if part.slot == .clock { .center } else if part.slot == .people or part.slot == .status or part.slot == .power { .trailing } else if part.slot == .space { .space } else { .leading }) {
                    bar_part(d, part)
                }
            }
        }
    }
}

/// Whether a part of the bar gives way on a bar narrower than a wide one: the strip and the
/// focused window's menu below an expanded bar (a phone, a tablet, or large text), and the
/// tasks, which run longest, below a wide one. The menu, the workspace, the clock, the status
/// and the person's own avatar always keep their room; of the people part, only the others here
/// give way (`bar_part`). The bar decides by its width, so a part that gives way leaves no room
/// behind; the bar's parts must never press into each other (the overview shows what gave way).
fn part_gives_way(part: BarPart, roomy: Bool) -> Bool {
    if part.slot == .tasks {
        return true
    }
    return not roomy and (part.slot == .strip or part.slot == .app_menu)
}

/// One part of the bar, drawn from what it is. The person's own avatar carries the question put
/// to them: their answer or their putting it off, never a banner over their windows, so it
/// stands on every bar; who else is here runs longest and shows on a wide bar alone. The status
/// part leads with the running applications' tray items, each under the app's own words for it.
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
        if d.strip != "" {
            ui.Button(d.strip, icon: .chevron_right, role: .bar) {
                on press: strips(d)
            }
        }
    } else if part.slot == .tasks {
        row(gap: .tight, align: .center) {
            for t in d.tasks {
                ui.Button(t.title, icon: .bot, role: .bar)
            }
        }
    } else if part.slot == .people {
        row(gap: .tight, align: .center) {
            if space.wide {
                for p in d.people {
                    person_chip(d, p)
                }
            }
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
            } else if d.person != "" {
                ui.Avatar(name: .some(d.person))
            }
        }
    } else if part.slot == .power {
        ui.Button("Power", icon: .battery, icon_only: true, role: .bar)
    } else if part.slot == .agent {
        for a in d.agents {
            agent_chip(d, a)
        }
    } else if part.slot == .status {
        row(gap: .tight, align: .center) {
            for item in d.tray {
                ui.Button(item.title, role: .bar) {
                    on press: open_tray_item(d, item)
                }
            }
            bell(d)
        }
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
            on choose: show_overview(d)
        }
    }
}

/// The device's status: an icon button named with what it shows — "Notifications", how many
/// are unread, or that they are silenced — with the count above it as a badge, so the name
/// assistive technology reads is the whole fact and the number is for the eye. Pressing it
/// opens or closes the device's status: the quick settings, or the notifications.
///
/// The count stands over the control's top trailing corner, in the button's own box: a badge
/// anchored to what it counts, never a sibling that takes room in the bar. Beside it in a row
/// the count pushed the bar's run past a narrow window and covered the button's own edge.
view bell(d: Desk) {
    let name = if d.quiet { "Notifications silenced" } else { "Notifications" }
    let label = if d.unread == 0 { name } else if d.unread == 1 { "{name}, 1 unread" } else { "{name}, {d.unread} unread" }
    layers {
        ui.Button(label, icon: if d.quiet { .bell_off } else { .bell }, icon_only: true, role: .bar) {
            on press: status(d)
        }
        if d.unread > 0 {
            row(at: .top_trailing) {
                ui.Badge(count: d.unread)
            }
        }
    }
}
