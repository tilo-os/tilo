app so.tilo.shell "Shell" (icon: .monitor, category: .system, records: .own)

use so.tilo.session as session

/// This device's desk: the time in the person's style, who they are, their notifications, the
/// workspaces on this device, the tasks running for them and the surface the desk shows over
/// its windows. The session keeps it in step; the shell's views
/// read it. Its text is the session's to replace whole, never edited here: each is a register,
/// not text to merge.
type Desk (origin: session.desk) {
    person: Text merge register
    /// What the person typed in the shell's search, which opens the launcher where they left off.
    spotlight: Text merge register
    /// The question the person is asked about themselves; empty when none waits.
    ask: Text merge register
    /// What answering the question brings.
    ask_detail: Text merge register
    /// The question's answer, as its button says it.
    ask_answer: Text merge register
    /// Whether the question stands open on the person's avatar.
    ask_open: Bool
    workspace: Text merge register
    strip: Text merge register
    workspaces: Collection<Workspace>
    people: Collection<Person>
}

/// A workspace on this device.
type Workspace (origin: session.workspace) {
    key: Text merge register
    name: Text merge register
    current: Bool
}

/// Someone else here in the workspace in front.
type Person (origin: session.person) {
    key: Text merge register
    name: Text merge register
}

root arrangement: Arrangement

root Desk

default view desk for Desk

default view desk_view for arrangement

default view shell_settings for Shell

/// The person's desk as a window or a display shows it: the desk style the person picked
/// (`Viewing.style`), drawing the arrangement. The bar reads `desk`, this package's other root,
/// as any view reads a root of the package it uses.
///
/// This is the arrangement's default view, so it is the one view a style is drawn *through* and
/// never one of the styles: a desk style is a view of the arrangement any installed package
/// declares (`shell.md` §6), found by the type its one parameter takes, never a keyword and never
/// this view's name. `show` draws the style `Viewing.style` names, in the package it names.
///
/// The traditional desktop is the one style that is the chrome's too: picking its layout
/// (`apply` of `windows_desktop`) lays the desk out that way, as picking it in the switch does.
///
/// A style that does not load — its package did not compile against the shell and the session,
/// or its view failed — draws the built-in strips instead, and the person's choice stays as it
/// was: the session marks the style `loads: false` with why, and raises a notice naming it
/// (`shell.md` §6, "Nothing can lock a person out"). Fixing the package flips it back, and the
/// desk shows the chosen style again without a restart.
view desk_view(a: Arrangement) {
    let d = root Desk
    let chosen = style_of(viewing)
    layers {
        // The chosen style's own settings, over whatever the style drew: the system's one
        // settings place for a desk style, opened from the bar's style menu (`style_menu`,
        // `strips.tl`) and drawn here rather than by each style, so every style — the shipped
        // four and any a person or agent wrote — gets settings at the same place and by the
        // same actions (`shell.md` §6, "Two ways to change a desk"). A style that draws no bar
        // is reached by Settings, which shows the same panel. A phone's desk is narrower than
        // the settings are wide, so there they take the desk rather than a panel over it.
        if shell.settings_open and space.compact {
            style_sheet(d)
        } else {
            if loads_style(d, chosen) {
                show(a, view: chosen, fill)
            } else {
                show(a, view: "so.tilo.shell:strips", fill)
            }
            if shell.settings_open {
                style_panel(d)
            }
        }
    }
}

/// Whether the style `chosen` loads, as the session publishes it (`DeskStyle.loads`): a style
/// the session has not listed yet is one it has said nothing against, so it is drawn and, if it
/// cannot be, the failed state the runtime draws stands in for it.
fn loads_style(d: Desk, chosen: Text) -> Bool {
    if let found = device.styles.offered.find(s -> s.style == chosen) {
        return found.loads
    }
    true
}

/// The time, to the minute, and the timer Clocks runs beside it: its label and what it has
/// left (a stopwatch: what it has run), paused when the person paused it. The name is a text of
/// its own, so a reader hears it; the reading is the number beside it, and a paused timer says
/// so.
view clock(d: Desk) {
    row(gap: .tight, align: .center) {
        ui.Text(device.time.clock, role: .subheading, numeric: true)
        if device.time.timer != "" {
            ui.Text(device.time.timer, role: .caption)
            if device.time.timer_paused {
                ui.Text("Paused", role: .caption)
            }
            ui.Text(device.time.timer_left, role: .caption, numeric: true)
        }
    }
}

/// The top bar: the parts the person put on it, in their order, each a view of its own, so a change
/// runs only the part that reads it. The bar is a run the person reorders by dragging a part, and a
/// part dragged onto it from the settings lands where it is dropped; letting go runs one action,
/// `place_part`. A compact bar keeps the clock, the workspace, the connection, the charge, the
/// notifications and the person's own face; who is here and the sound give way there, and a
/// question put to the person always keeps its seat.
view bar(d: Desk) {
    let s = shell
    let roomy = space.width_class == .expanded or space.width_class == .wide
    let nobody = d.ask == "" and space.compact
    ui.DragRun(axis: .horizontal, name: "Top bar") {
        on drop(moved: BarPart, at: Int): place_part(s, moved, at)
        for part in s.bar_parts {
            if part.shown
                and (space.wide or not part_gives_way(part, roomy))
                and not (part.slot == .people and nobody) {
                ui.DragItem(
                    part,
                    title: part.title,
                    group: if part.slot == .clock {
                        .center
                    } else if part.slot == .people or part.slot == .status {
                        .trailing
                    } else if part.slot == .space { .space } else { .leading },
                ) {
                    bar_part(d, part)
                }
            }
        }
    }
}

/// Whether a part of the bar gives way on a bar narrower than a wide one: the strip and the
/// focused window's menu below an expanded bar (a phone, a tablet, or large text), and the
/// tasks and the agents, which run longest, below a wide one. The menu, the workspace, the clock,
/// the status and a question put to the person always keep their room — the menu alone on a bar
/// narrower than a compact one, where its preset switch gives way (`bar_part`). The bar decides
/// by its width, so a part that gives way leaves no room behind; the bar's parts must never press
/// into each other (the overview shows what gave way).
fn part_gives_way(part: BarPart, roomy: Bool) -> Bool {
    if part.slot == .tasks or part.slot == .agent {
        return true
    }
    return not roomy and (part.slot == .strip or part.slot == .app_menu)
}

/// Whether the bar is too narrow to hold its parts at their roomiest: a phone, or a short window
/// with large text. Room enough for the menu, the workspace's name, the clock and the status
/// beside each other starts at about this width; below it the parts pack a tighter gap apart, the
/// workspace keeps only its icon and the preset switch gives way, so the person's own face at the
/// end never goes off the bar.
fn bar_is_narrow(width: Float) -> Bool {
    return width < 26.0
}

/// One part of the bar, drawn from what it is. Who is here is its faces alone, one button that
/// opens the roster of everyone here and names them to assistive technology (`people_button`);
/// a question put to the person stands beside them, their answer or their putting it off, never
/// a banner over their windows. The status part leads with the running applications' tray
/// items, each under the app's own words for it, then the device's own status, the notifications,
/// and the person's own face at the far end. On a compact bar the tray items and the sound give
/// way: what a glance needs — the clock, the workspace, the connection, the charge, the
/// notifications and the person's own face — keeps its room.
view bar_part(d: Desk, part: BarPart) {
    if part.slot == .space {
        spacer
    } else if part.slot == .clock {
        clock(d)
    } else if part.slot == .menu {
        row(gap: .related, align: .center) {
            ui.Button("Tilo", icon: .tilo, icon_only: true, role: .bar) {
                on press: session.overview()
            }
            // The switch between the desk styles, in the bar's own seat rather than in a row of
            // its own, so nothing stands between a window and the bar. A bar too narrow for it
            // beside the clock and the status gives it up, as it gives up the strip and the
            // focused window's menu; the same switch is a page of Settings, so the style a
            // person picked is never out of reach, and no control is pressed into its neighbour
            // to keep it here.
            if not bar_is_narrow(space.width) {
                style_menu(viewing, d)
            }
        }
    } else if part.slot == .workspace {
        workspace_menu(d)
    } else if part.slot == .app_menu {
        action_bar(arrangement, BarRoom.menus)
    } else if part.slot == .strip {
        if d.strip != "" {
            ui.Button(d.strip, icon: .chevron_right, role: .bar)
        }
    } else if part.slot == .people {
        row(gap: .related, align: .center) {
            if not space.compact {
                people_button(d)
            }
            if d.ask != "" {
                ui.Button(d.ask, role: .bar)
                if d.ask_answer != "" {
                    ui.Button(d.ask_answer, role: .bar)
                }
            }
        }
    } else if part.slot == .agent {
        for a in device.agents.seated {
            agent_chip(d, a)
        }
    } else if part.slot == .status {
        // One cluster, so its controls stand a cluster gap apart and never the tightest: a bar
        // control's press target is wider than its face (a finger's, and a mouse's), and two of
        // them at the tightest gap would share the press between them. What the device does not
        // publish is not drawn at all, so no gap stands for a control that is not there.
        row(gap: .related, align: .center) {
            if not space.compact and not device.tray.items.is_empty() {
                row(gap: .related, align: .center) {
                    for item in device.tray.items {
                        ui.Button(item.title, role: .bar)
                    }
                }
            }
            // The device's own status: the sound's level, the connection the machine's traffic
            // leaves on, and the pack's charge. Each reads the resource the session publishes —
            // the sound, the network and the battery `quick.tl` reads and changes too, so the bar
            // and the panel show one fact, never a copy — and each press opens the quick settings,
            // the one place a person changes them.
            if not space.compact {
                volume_status()
            }
            network_status()
            battery_status()
            bell()
            if d.person != "" {
                ui.Avatar(name: d.person)
            }
        }
    }
}

/// The workspace in front, opening the workspaces to switch between and the actions on this
/// device's workspaces. On a bar too narrow for its name beside the clock and the status (a
/// phone with large text), it is the workspaces' icon, named for the workspace; a desk whose
/// workspace the session has not published a name for still opens by the workspaces' name.
view workspace_menu(d: Desk) {
    let name = if d.workspace != "" { d.workspace } else { "Workspaces" }
    let icon_only = bar_is_narrow(space.width)
    ui.MenuButton(name, icon: if icon_only { .layers }, icon_only: icon_only, role: .bar) {
        for w in d.workspaces {
            ui.MenuItem(w.name, checked: w.current) {
                on choose: device.switch_workspace(device.desk, workspace: w.key)
            }
        }
        ui.MenuItem("New workspace", icon: .plus) {
            on choose: session.new_workspace()
        }
        ui.MenuItem("Join a workspace", icon: .users) {
            on choose: session.join_workspace()
        }
        ui.MenuItem("Workspace settings", icon: .settings) {
            on choose: session.workspace_settings()
        }
        ui.MenuItem("Overview", icon: .layout_grid) {
            on choose: session.overview()
        }
    }
}

/// The sound's level, and that it is muted; nothing on a device whose sound reads no level.
/// A compact bar keeps the connection and the charge, what a glance looks for; the sound is a
/// key away and a press on the others opens it.
view volume_status() {
    let sound = device.sound
    if let volume = sound.volume {
        // Rounded, not cut: a level of 0.65 is 65%.
        let percent = (volume * 100.0 + 0.5).to_int()
        ui.Button(
            if sound.muted { "Volume, muted" } else { "Volume, {percent}%" },
            icon: if sound.muted { .volume_x } else { .volume_2 },
            icon_only: true,
            role: .bar,
        ) {
            on press: device.show_surface(device.overlays, surface: "quick-settings")
        }
    }
}

/// The connection the machine's traffic leaves on: Wi-Fi under its network's name, a cable, or
/// offline; nothing on a device that publishes no connection at all.
view network_status() {
    let link = device.network.link
    if let joined = link {
        let label = if joined.kind == .wifi {
            if let called = joined.name { "Wi-Fi, {called}" } else { "Wi-Fi" }
        } else if joined.kind == .wired {
            if let called = joined.name { "Wired, {called}" } else { "Wired" }
        } else { "Offline" }
        ui.Button(
            label,
            icon: if joined.kind == .wifi {
                .wifi
            } else if joined.kind == .wired { .ethernet_port } else { .wifi_off },
            icon_only: true,
            role: .bar,
        ) {
            on press: device.show_surface(device.overlays, surface: "quick-settings")
        }
    }
}

/// The pack's charge as a percentage beside the battery, its glyph the charging battery while
/// the pack takes charge (or waits for it) and the plain battery otherwise. It is the power
/// service's own state, not whether the machine happens to run on the pack, and nothing on a
/// machine with no battery. A compact bar shows the glyph alone. The glyph says the charging,
/// so the reading is the charge alone and the name assistive technology hears says both.
view battery_status() {
    let b = device.battery
    if b.present {
        let charging = b.state == "charging" or b.state == "pending-charge"
        let name = if charging { "{b.percentage}%, charging" } else { "{b.percentage}%" }
        ui.Button(
            "{b.percentage}%",
            accessible_name: name,
            icon: if charging { .battery_charging } else { .battery },
            icon_only: space.compact,
            role: .bar,
        ) {
            on press: device.show_surface(device.overlays, surface: "quick-settings")
        }
    }
}

/// The notifications: an icon button named with what it shows ("Notifications", how many are
/// unread, or that they are silenced), with the count on the bell's corner. Pressing it opens
/// the notification centre, or closes it while it is up: the bell is what it is named for, and
/// the quick settings are the sound's, the connection's and the battery's own, beside it.
///
/// The count is the button's own (`count:`): it stands on the bell's top trailing corner and
/// wholly inside the button's face, so the bar's own edge never cuts it and it covers neither
/// the bell nor the control beside it.
view bell() {
    let quiet = (device.notifications.quiet ?? "") != ""
    let name = if quiet { "Notifications silenced" } else { "Notifications" }
    let waiting = unread()
    let label = if waiting == 0 {
        name
    } else if waiting == 1 { "{name}, 1 unread" } else { "{name}, {waiting} unread" }
    ui.Button(
        label,
        icon: if quiet { .bell_off } else { .bell },
        icon_only: true,
        role: .bar,
        count: waiting,
    ) {
        on press: device.show_surface(device.overlays, surface: "centre")
    }
}
