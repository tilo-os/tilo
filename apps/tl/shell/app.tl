app so.tilo.shell "Shell" (icon: .monitor, category: .system, records: .own)

use so.tilo.lang.chat as chat
use so.tilo.lang.notes as notes
use so.tilo.lang.tasks as tasks
use so.tilo.session as session

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
    /// The runs of what the person asked the session's agents, oldest first; each is a
    /// conversation (`conversation.tl`).
    runs: Collection<Run>
    run_entries: Collection<RunEntry>
    entry_refs: Collection<EntryRef>
}

/// A run of what the person asked one of the session's agents: the conversation they have in
/// it.
type Run (origin: session.run) {
    key: Text merge register
    title: Text merge register (title)
    agent: Text merge register
    phase: Text merge register
    active: Bool
    /// What the agent says now, as the model streams it; empty between turns.
    live: Text merge register
}

/// One entry of a run's conversation: what the person or the agent said, or a program either
/// of them ran.
type RunEntry (origin: session.run_entry) {
    key: Text merge register
    run: Text merge register
    seq: Int
    author: Text merge register
    by_agent: Bool
    step: Bool
    /// The words, or the program as it ran.
    text: Text merge register (title)
    outcome: Text merge register
    answer: Text merge register
}

/// A resource a program in a conversation printed (a reference) or changed (a row to undo).
type EntryRef (origin: session.entry_ref) {
    key: Text merge register
    entry: Text merge register
    changed: Bool
    resource: Text merge register
    app_id: Text merge register
    type_kind: Text merge register
    label: Text merge register (title)
    change: Text merge register
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

/// The person's desk as a window shows it: the top bar over the preset they picked, and the
/// preset switch beside the bar's actions — or, for the traditional desktop, the whole screen:
/// its frames on the desk under the desk's own bar and dock, as the session shows them on an
/// output. The bar reads `desk`, this package's other root, as
/// any view reads a root of the package it uses. A change of the arrangement runs the preset
/// body; a change of the desk reading runs the bar.
view desk_view(a: Arrangement) {
    let v = viewing
    if v.showing == .desktop {
        if space.compact {
            desk_around(root Desk, .some(a), fill)
        } else {
            layers(fill) {
                traditional_desktop(a, fill)
                desk(root Desk, fill)
            }
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
            on press: session.toggle_tilo_menu()
        }
    } else if part.slot == .workspace {
        workspace_menu(d)
    } else if part.slot == .app_menu {
        action_bar(arrangement, BarRoom.menus)
    } else if part.slot == .strip {
        if d.strip != "" {
            ui.Button(d.strip, icon: .chevron_right, role: .bar)
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
                ui.Button(d.ask, role: .bar)
                if d.ask_answer != "" {
                    ui.Button(d.ask_answer, role: .bar)
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
                ui.Button(item.title, role: .bar)
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
                on choose: session.switch_workspace(workspace: w.key)
            }
        }
        ui.MenuItem("New workspace", icon: .plus) {
            on choose: session.new_workspace(room: .none, name: .none)
        }
        ui.MenuItem("Join a workspace", icon: .users) {
            on choose: session.join_workspace()
        }
        ui.MenuItem("Workspace settings", icon: .settings) {
            on choose: session.workspace_settings()
        }
        ui.MenuItem("Overview", icon: .layout_grid) {
            on choose: session.toggle_overview()
        }
    }
}

/// The device's status: an icon button named with what it shows — "Notifications", how many
/// are unread, or that they are silenced — with the count on the bell's corner, so the name
/// assistive technology reads is the whole fact and the number is for the eye. Pressing it
/// opens or closes the device's status: the quick settings, or the notifications.
///
/// The count is the button's own (`count:`): it stands centred on the bell's top trailing
/// corner, and the button keeps room for it, so it covers neither the bell nor the control
/// beside it.
view bell(d: Desk) {
    let name = if d.quiet { "Notifications silenced" } else { "Notifications" }
    let label = if d.unread == 0 { name } else if d.unread == 1 { "{name}, 1 unread" } else { "{name}, {d.unread} unread" }
    ui.Button(label, icon: if d.quiet { .bell_off } else { .bell }, icon_only: true, role: .bar, count: d.unread) {
        on press: session.toggle_quick_settings()
    }
}
