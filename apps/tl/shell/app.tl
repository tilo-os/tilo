app so.tilo.shell "Shell" (icon: .monitor, category: .system, resources: .own)

use so.tilo.session as session
use desktop as desktop
use tilo.styles as styles

root layout: Layout

default view shell_view for layout

default view shell_settings for Shell

/// The person's layout as a window or a display shows it: the style the person picked
/// (`Viewing.style`), drawing the layout.
///
/// This is the layout's default view, so it is the one view a style is drawn *through* and
/// never one of the styles: a style is a view of the layout any installed package
/// declares (`shell.md` §6), found by the type its one parameter takes, never a keyword and never
/// this view's name. `show` draws the style `Viewing.style` names, in the package it names.
///
/// The traditional desktop is the one style that is the chrome's too: picking its layout
/// (`apply` of `windows_desktop`) lays the shell out that way, as picking it in the switch does.
///
/// A style that does not load — its package did not compile against the shell and the session,
/// or its view failed — draws the built-in strips instead, and the person's choice stays as it
/// was: the session marks the style `loads: false` with why, and raises a notice naming it
/// (`shell.md` §6, "Nothing can lock a person out"). Fixing the package flips it back, and the
/// shell shows the chosen style again without a restart.
view shell_view(a: Layout) window "Desktop shell" {
    let chosen = style_of(viewing)
    layers {
        // A secondary press on the shell, where nothing of it took the press, is the workspaces'
        // own menu at the pointer: the same rows the bar's workspace button opens, so a person
        // reaches their workspaces anywhere on the shell.
        ui.ContextMenu {
            for w in layout.workspaces {
                ui.MenuItem(w.name, checked: w.current) {
                    on choose: session.switch_workspace(workspace: w.key)
                }
            }
            ui.MenuItem("New workspace", icon: .plus) {
                on choose: session.new_workspace()
            }
            ui.MenuItem("Join a workspace", icon: .users) {
                on choose: session.join_workspace()
            }
            ui.MenuItem("Invite someone", icon: .user_plus) {
                on choose: device.show_invite(device.invite)
            }
            ui.MenuItem("Workspace settings", icon: .settings) {
                on choose: session.workspace_settings()
            }
            ui.MenuItem("Overview", icon: .layout_grid) {
                on choose: session.overview()
            }
        }
        // The chosen style's own settings, over whatever the style drew: the system's one
        // settings place for a style, opened from the bar's style menu (`style_menu`,
        // `strips.tl`) and drawn here rather than by each style, so every style — the shipped
        // four and any a person or agent wrote — gets settings at the same place and by the
        // same actions (`shell.md` §6). A style that draws no bar
        // is reached by Settings, which shows the same panel. A phone's shell is narrower than
        // the settings are wide, so there they take the screen rather than a panel over it.
        if shell.settings_open and space.compact {
            style_sheet()
        } else {
            if loads_style(chosen) {
                show(a, view: chosen, fill)
            } else {
                show(a, view: "so.tilo.shell:strips", fill)
            }
            if shell.settings_open {
                style_panel()
            }
        }
        // What the person keeps on the shell sits over the style and under every surface, its
        // own cards below a settings panel or a sheet: a display too narrow for a strip and a
        // panel side by side has room for neither widget.
        if not space.compact {
            widget_layer(a)
        }
    }
}

/// Whether the style `chosen` loads, as the device lists it and this session marks it
/// (`Style.loads`): a style not listed yet is one nothing has said anything against, so it is
/// drawn and, if it cannot be, the failed state the runtime draws stands in for it.
fn loads_style(chosen: Text) -> Bool {
    if let found = styles.installed.offered.find(s -> s.style == chosen) {
        return found.loads
    }
    true
}

/// The time, to the minute, and the timer Clocks runs beside it: its label and what it has
/// left. What runs now — a call, a timer — is a live activity, which the shell shows from the live
/// views the index derives. The time's name is a text of its own, so a reader hears it.
view clock() {
    row(gap: .tight, align: .center) {
        ui.Text(device.time.clock, role: .subheading, numeric: true, lines: 1)
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
/// question put to the person always keeps its place.
///
/// On the canvas the canvas is the whole screen (`canvas.md` §7): the bar keeps only what a
/// person needs on any screen — the time, the notices and the person — and a question put to
/// the person. The style and the workspaces are in the person's own menu and in Settings,
/// not in a control that is always there (`canvas_keeps`).
view bar() {
    let s = shell
    let roomy = space.width_class == .expanded or space.width_class == .wide
    // The display in use carries the whole bar; each other display carries the compact one, as
    // the desktop's own bars do: what a person reaches for where their work is (the sounds, the
    // connection, the charge, the notices, the people) stays where they work.
    let compact = not on_primary()
    let nobody = device.overlays.ask == "" and (space.compact or frames_style())
    ui.DragRun(axis: .horizontal, name: "Top bar") {
        on drop(moved: BarPart, at: Int): place_part(s, moved, at)
        for part in s.bar_parts {
            if part.shown
                and (space.wide or not part_gives_way(part, roomy))
                and (not compact or part_keeps_compact(part.kind))
                and not (part.kind == .people and nobody)
                and (not frames_style() or canvas_keeps(part)) {
                ui.DragItem(
                    part,
                    title: part_name(part.kind),
                    group: if part.kind == .clock {
                        .center
                    } else if part.kind == .people or part.kind == .status {
                        .trailing
                    } else if part.kind == .spacer { .space } else { .leading },
                ) {
                    bar_part(part)
                }
            }
        }
    }
}

/// Whether the part draws the display in use: the one the person made primary. A view drawn
/// with no display reading (a shot, a test) is the display in use, so a shell that is not told
/// which display it stands on draws the whole bar rather than a compact one.
fn on_primary() -> Bool {
    return device.screen.connector == "" or device.screen.primary
}

/// Whether a part stays on a display that is not the one in use: a compact bar keeps what a
/// glance needs anywhere (the system's menu, the workspace, the strip, the time and the gap
/// that spreads them), and gives up what serves the person where they work (the window's menus,
/// the sounds, the connection, the charge, the notices, the people and the running work).
fn part_keeps_compact(kind: BarPartKind) -> Bool {
    return kind == .menu
        or kind == .workspace
        or kind == .strip
        or kind == .clock
        or kind == .spacer
}

/// Whether the canvas's bar keeps a part (`canvas.md` §7, "The canvas is the whole screen"): the
/// time, the status cluster (which on the canvas is the notices and the person, `bar_part`), a
/// question put to the person, and the spacer that spreads them.
fn canvas_keeps(part: BarPart) -> Bool {
    part.kind == .clock or part.kind == .status or part.kind == .people or part.kind == .spacer
}

/// Whether a part of the bar gives way on a bar narrower than a wide one: the strip and the
/// focused window's menu below an expanded bar (a phone, a tablet, or large text), and the
/// tasks and the agents, which run longest, below a wide one. The menu, the workspace, the clock,
/// the status and a question put to the person always keep their room — the menu alone on a bar
/// narrower than a compact one, where its preset switch gives way (`bar_part`). The bar decides
/// by its width, so a part that gives way leaves no room behind; the bar's parts must never press
/// into each other (the overview shows what gave way).
fn part_gives_way(part: BarPart, roomy: Bool) -> Bool {
    if part.kind == .tasks or part.kind == .agent {
        return true
    }
    return not roomy and (part.kind == .strip or part.kind == .app_menu)
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
view bar_part(part: BarPart) {
    if part.kind == .spacer {
        spacer
    } else if part.kind == .clock {
        clock()
    } else if part.kind == .menu {
        row(gap: .related, align: .center) {
            ui.MenuButton("Tilo", icon: .tilo, icon_only: true, role: .bar) {
                ui.MenuItem("Overview", icon: .layout_grid) {
                    on choose: session.overview()
                }
                ui.MenuItem("Undo layout change", icon: .undo_2) {
                    on choose: session.undo()
                }
                ui.MenuItem("Redo layout change", icon: .redo_2) {
                    on choose: session.redo()
                }
                ui.MenuItem("Settings", icon: .settings) {
                    on choose: session.open_settings(link: "settings://system")
                }
                ui.MenuItem("About this computer", icon: .info) {
                    on choose: session.about(open: true)
                }
            }
            // The switch between the styles, in the bar itself rather than in a row of
            // its own, so nothing stands between a window and the bar. A bar too narrow for it
            // beside the clock and the status gives it up, as it gives up the strip and the
            // focused window's menu; the same switch is a page of Settings, so the style a
            // person picked is never out of reach, and no control is pressed into its neighbour
            // to keep it here.
            if not bar_is_narrow(space.width) {
                style_menu(viewing)
            }
        }
    } else if part.kind == .workspace {
        workspace_menu()
    } else if part.kind == .app_menu {
        action_bar(layout, BarRoom.menus)
    } else if part.kind == .strip {
        strip_part()
    } else if part.kind == .people {
        row(gap: .related, align: .center) {
            if not space.compact and not frames_style() {
                people_button()
            }
            // Whose view this is while the person follows someone: their face and name where the
            // bar's glance value stands, with the way to stop.
            if not space.compact {
                follow_chip()
            }
            wave_notice()
            arrival_notice()
            sharing_indicator()
        }
    } else if part.kind == .agent {
        for a in device.agents.sessions {
            agent_chip(a)
        }
    } else if part.kind == .tasks {
        // What works for the person now: each agent session here, as its own chip.
        row(gap: .related, align: .center) {
            for a in device.agents.sessions {
                agent_chip(a)
            }
        }
    } else if part.kind == .status and frames_style() {
        // The canvas's bar: the notices and the person, whose menu holds the style and the
        // workspaces the canvas does not keep on its bar.
        row(gap: .related, align: .center) {
            bell()
            person_menu()
        }
    } else if part.kind == .status {
        // One cluster, so its controls stand a cluster gap apart and never the tightest: a bar
        // control's press target is wider than its face (a finger's, and a mouse's), and two of
        // them at the tightest gap would share the press between them. What the device does not
        // publish is not drawn at all, so no gap stands for a control that is not there.
        row(gap: .related, align: .center) {
            // What is live now — a video playing, a timer running — by its own view, on every
            // bar, compact or not: it is what a glance at the bar is for while it lasts.
            live_pill()
            if not space.compact and not device.tray.items.is_empty() {
                row(gap: .related, align: .center) {
                    for item in device.tray.items {
                        // A press opens the item's window, or its menu when it is one: the app's
                        // own, placed by the app. The app's unread count rides the control, and a
                        // pulsing mark says it wants the person.
                        ui.Button(
                            item.title,
                            role: .bar,
                            count: item.badge,
                            alert: item.attention,
                        ) {
                            on press: device.open_tray_item(device.tray, key: item.key)
                        }
                    }
                }
            }
            // What is using the camera or the microphone now: a glyph each, naming the first app
            // in reach and how many more, and a press that opens the quick settings where sound
            // and capture are changed.
            capture_status()
            // The device's own status: the sound's level, the connection the machine's traffic
            // leaves on, and the pack's charge. Each reads the resource the session publishes —
            // the sound, the network and the battery `quick.tl` reads and changes too, so the bar
            // and the panel show one fact, never a copy — and each press opens the quick settings,
            // the one place a person changes them.
            // Below a wide bar, what is live takes the sound's place: the sound is in quick
            // settings, a press away, as on a compact bar.
            if not space.compact and not (live_on_bar() and space.width_class != .wide) {
                volume_status()
            }
            if not space.compact or folders_need_person(device.folders.folders) {
                sync_status()
            }
            network_status()
            battery_status()
            bell()
            // The person's own face opens their own card: their name, status and colour, their
            // devices, pairing a new one and Settings. A question put to them shows as its dot
            // here, and their card is where it is asked properly; a person the session has not
            // named yet still has a face, since that is where the name is set.
            ui.Button(
                if device.overlays.ask != "" {
                    std.say("{device.overlays.ask}: open your card")
                } else if layout.person != "" {
                    std.say("{layout.person}: open your card")
                } else { std.say("You: open your card") },
                faces: [invoker],
                dot: device.overlays.ask != "",
                role: .bar,
            ) {
                on press: device.show_person(device.people, person: "self")
            }
        }
    }
}

/// The workspace in front, opening the workspaces to switch between and the actions on this
/// device's workspaces. On a bar too narrow for its name beside the clock and the status (a
/// phone with large text), it is the workspaces' icon, named for the workspace; a shell whose
/// workspace the session has not published a name for still opens by the workspaces' name.
view workspace_menu() {
    state renaming: Text = ""
    state renamed: Text = ""
    let name = if front_workspace_name() != "" { front_workspace_name() } else { "Workspaces" }
    let current = layout.workspaces.find(w -> w.current)
    if renaming != "" {
        ui.TextInput(
            bind: renamed,
            placeholder: layout.workspaces.find(w -> w.key == renaming)?.name ?? "Workspace name",
            autofocus: true,
        ) {
            on submit: rename_workspace(renaming, renamed).then(set(renaming, ""))
            on cancel: set(renaming, "")
        }
    } else {
        // On a phone the status cluster takes the name's room while a folder needs the person,
        // and below a wide bar so does something live.
        let icon_only = bar_is_narrow(space.width)
            or space.compact and folders_need_person(device.folders.folders)
            or live_on_bar() and space.width_class != .wide
        ui.MenuButton(name, icon: if icon_only { .layers }, icon_only: icon_only, role: .bar) {
            for w in layout.workspaces {
                if w.home != "" {
                    // Where the shell is a page, each workspace is a page of its own: the menu
                    // leads to it as the workspaces' own cards do (`workspaces.tl`).
                    ui.MenuItem(w.name, checked: w.current, link: w.home)
                } else {
                    ui.MenuItem(w.name, checked: w.current) {
                        on choose: session.switch_workspace(workspace: w.key)
                    }
                }
            }
            ui.MenuItem("New workspace", icon: .plus) {
                on choose: session.new_workspace()
            }
            ui.MenuItem("Join a workspace", icon: .users) {
                on choose: session.join_workspace()
            }
            ui.MenuItem("Invite someone", icon: .user_plus) {
                on choose: device.show_invite(device.invite)
            }
            if let cur = current {
                ui.MenuItem("Rename {cur.name}", icon: .pencil) {
                    on choose: set(renaming, cur.key).then(set(renamed, ""))
                }
                ui.MenuItem("Delete {cur.name}", icon: .trash, destructive: true) {
                    on choose: session.delete_workspace(workspace: cur.key)
                }
            }
            ui.MenuItem("Workspace settings", icon: .settings) {
                on choose: session.workspace_settings()
            }
            ui.MenuItem("Overview", icon: .layout_grid) {
                on choose: session.overview()
            }
            // The workspace's history is the system's, drawn by the Activity app: the menu opens that
            // window on this workspace, so what happened, who did it and what can be taken back stand
            // where the person expects the system's own views.
            ui.MenuItem("History", icon: .history) {
                on choose: session.activity(workspace: front_workspace(), view: "explore")
            }
        }
    }
}

/// Name the workspace `key` what the person typed: the workspace's own published `rename`, so the
/// name reaches every device and every list of it. A blank name is nothing to do.
action rename_workspace(key: Text, name: Text) {
    if name.trim() != "" {
        session.rename(workspace: key, name: name.trim())
    }
}

/// The strip shown, at the bar's left end, as a menu of the workspace's strips under it: the one
/// press that switches strip, a new one, and — for the strip in front — renaming it, removing it
/// when it is empty, and moving it along the workspace. Every change is one of the session's
/// arranging actions (`shell:layout`), so a press, a script and an agent make the same change.
///
/// Renaming types over the button where it stands, rather than in a sheet: the strip's name is
/// what the person is looking at, and Enter keeps it.
view strip_part() {
    state renaming: Text = ""
    state renamed: Text = ""
    let workspace = front_workspace()
    let strips = layout.rows.filter(r -> r.workspace == workspace)
    let current = strips.find(r -> r.current)
    if renaming != "" {
        ui.TextInput(
            bind: renamed,
            placeholder: strips.find(r -> r.key == renaming)?.name ?? "Strip name",
            autofocus: true,
        ) {
            on submit: rename_strip(renaming, renamed).then(set(renaming, ""))
            on cancel: set(renaming, "")
        }
    } else if front_strip_name() != "" {
        ui.MenuButton(front_strip_name(), icon: .chevron_right, role: .bar) {
            for r in strips {
                ui.MenuItem(r.name, checked: r.current, icon: .columns_2) {
                    on choose: session.show_strip(workspace: workspace, strip: r.key)
                }
            }
            ui.MenuItem("New strip", icon: .plus) {
                on choose: session.new_strip()
            }
            if let cur = current {
                ui.MenuItem("Rename {cur.name}", icon: .pencil) {
                    on choose: set(renaming, cur.key).then(set(renamed, ""))
                }
                if strips.len() > 1 {
                    // Its windows are carried into the strip after it, so deleting it takes
                    // nothing from the person.
                    ui.MenuItem("Delete {cur.name}", icon: .trash, destructive: true) {
                        on choose: session.delete_strip(strip: cur.key)
                    }
                }
                if let before = strips.previous(cur) {
                    ui.MenuItem("Move {cur.name} left", icon: .arrow_left) {
                        on choose: session.move_strip(strip: cur.key, before: before.key)
                    }
                }
                if let at = strips.index_of(cur) {
                    if let after = strips.at(at + 2) {
                        ui.MenuItem("Move {cur.name} right", icon: .arrow_right) {
                            on choose: session.move_strip(strip: cur.key, before: after.key)
                        }
                    }
                }
            }
        }
    }
}

/// The strips list: every strip of the workspace in front, to switch to, and a new one. It stands
/// at the shell's leading edge while `show-strips` asks for it, as the strip switcher does, and
/// choosing a strip closes it; Done puts it away without switching.
view strips_list() {
    let workspace = front_workspace()
    ui.Card(title: "Strips") {
        stack(gap: .tight) {
            for r in layout.rows.filter(r -> r.workspace == workspace) {
                ui.ListRow(r.name, icon: .columns_2, selected: r.current) {
                    on activate: session.show_strip(workspace: workspace, strip: r.key)
                }
            }
            ui.Button("New strip", icon: .plus) {
                on press: session.new_strip()
            }
            ui.Button("Done", role: .plain) {
                on press: session.show_strip(workspace: workspace, strip: front_row_key())
            }
        }
        // The workspace's history is the system's, drawn by the Activity app: the menu opens that
        // window on this workspace, so what happened, who did it and what can be taken back stand
        // where the person expects the system's own views.
        ui.MenuItem("History", icon: .history) {
            on choose: session.activity(workspace: front_workspace(), view: "explore")
        }
    }
}

/// The key of the row in front of the workspace in front: what showing it again answers, and how
/// the strips list puts itself away without switching away.
fn front_row_key() -> Text {
    let w = front_workspace()
    return layout.rows.find(r -> r.workspace == w and r.current)?.key ?? ""
}

/// Whether the strip `row` holds no window: removing it takes nothing from the person, and the
/// session refuses a strip that still does.
fn strip_is_empty(row: Text) -> Bool {
    return layout.strips
        .filter(s -> s.row == row)
        .all(s -> layout.places.filter(p -> p.strip == s.key).is_empty())
}

/// Name the strip `key` what the person typed: one arranging action of the session's, so a
/// script and an agent rename as a press does. A blank name is nothing to do.
action rename_strip(key: Text, name: Text) {
    if name.trim() != "" {
        session.rename(strip: key, name: name.trim())
    }
}

/// The person's own menu, as their face on the bar: the styles to show the shell as, the
/// chosen style's settings, and the workspaces to switch between, folded under one row. Where
/// the bar keeps no style switch and no workspace menu of its own (the canvas), this is where a
/// person finds them; Settings has the same.
view person_menu() {
    let v = viewing
    let me = if layout.person != "" { layout.person } else { "You" }
    let face = invoker
    ui.MenuButton("{me}: style and workspaces", faces: [face], role: .bar) {
        for style in styles.installed.offered {
            if style_listed(style) {
                ui.MenuItem(style_label(style), checked: v.style == style.style) {
                    on choose: show_style(v, style.style)
                }
            }
        }
        ui.MenuItem("Settings for {style_title(v)}", icon: .settings) {
            on choose: open_style_settings(shell)
        }
        for w in layout.workspaces {
            ui.MenuItem(w.name, checked: w.current, group: "Workspaces") {
                on choose: session.switch_workspace(workspace: w.key)
            }
        }
        ui.MenuItem("New workspace", icon: .plus, group: "Workspaces") {
            on choose: session.new_workspace()
        }
        ui.MenuItem("Join a workspace", icon: .users, group: "Workspaces") {
            on choose: session.join_workspace()
        }
        ui.MenuItem("Workspace settings", icon: .settings, group: "Workspaces") {
            on choose: session.workspace_settings()
        }
        ui.MenuItem("Overview", icon: .layout_grid, group: "Workspaces") {
            on choose: session.overview()
        }
        ui.MenuItem("History", icon: .history, group: "Workspaces") {
            on choose: session.activity(workspace: front_workspace(), view: "explore")
        }
        ui.MenuItem("About Tilo", icon: .info, group: "Tilo") {
            on choose: session.about()
        }
        ui.MenuItem("Who is here", icon: .users, group: "People") {
            on choose: device.show(device.people)
        }
        ui.MenuItem("Your card", icon: .user, group: "People") {
            on choose: device.show_person(device.people, person: "self")
        }
    }
}

/// What is using the camera or the microphone right now, as the bar's capture indicator: the
/// glyph of what is watched and of what is heard, naming the first app in reach and how many more,
/// and a press that opens the quick settings, where the sound and the capture are changed. Nothing
/// while nothing takes either.
view capture_status() {
    let uses = device.sound.uses
    if not uses.is_empty() {
        let listening = uses.any(u -> u.kind == .microphone)
        let watching = uses.any(u -> u.kind == .camera)
        let first = uses.first()
        let more = uses.len() - 1
        let label = if let taking = first {
            if more == 0 {
                "Watching {taking.label}"
            } else {
                "Watching {taking.label} and {more} more"
            }
        } else { "Watching" }
        ui.Button(
            label,
            icon: if watching { .camera } else { .mic },
            icon_only: space.compact,
            role: .bar,
            alert: listening,
        ) {
            on press: device.show_surface(device.overlays, surface: "quick-settings")
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

/// Whether what a person puts in their workspaces' folders on this computer has reached the
/// server keeping each workspace: the glyph of the folder that needs the person most, named in
/// full for assistive technology, opening every folder's state. Files that never reach the server
/// while the person believes they did are lost when this computer is, so a folder that is not in
/// sync says so here, where the person looks, and why. Nothing while this computer keeps no
/// folder.
view sync_status() {
    let folders = device.folders.folders
    if let first = folders.first() {
        let shown = needing_most(folders) ?? first
        let f = shown
        ui.MenuButton(
            folder_line(f),
            icon: if f.state == "stuck" {
                .triangle_alert
            } else if f.state == "sending" {
                .refresh
            } else if f.state == "content-going-out" {
                .upload
            } else if f.state == "paused" {
                .pause
            } else if f.state == "opening" or f.state == "waiting-for-workspace" {
                .hourglass
            } else { .cloud },
            icon_only: true,
            role: .bar,
        ) {
            for f in folders {
                ui.MenuItem(
                    folder_line(f),
                    icon: if f.state == "stuck" {
                        .triangle_alert
                    } else if f.state == "sending" {
                        .refresh
                    } else if f.state == "content-going-out" {
                        .upload
                    } else if f.state == "paused" {
                        .pause
                    } else if f.state == "opening" or f.state == "waiting-for-workspace" {
                        .hourglass
                    } else { .cloud },
                ) {
                    on choose: session.open_app(app: "so.tilo.file-explorer")
                }
            }
        }
    }
}

/// Whether a folder needs the person to look: its changes wait, or it is paused. A phone's bar
/// keeps the folders' glyph only then; the folders that are in sync or sending need no glance.
fn folders_need_person(folders: List<device.Folder>) -> Bool {
    return folders.any(f -> f.state == "stuck" or f.state == "paused")
}

/// The folder a person most needs to hear about: one whose changes wait, then one sending, then
/// one whose files aren't all on the server yet, then one still opening.
fn needing_most(folders: List<device.Folder>) -> Option<device.Folder> {
    return folders.find(f -> f.state == "stuck")
        ?? folders.find(f -> f.state == "sending")
        ?? folders.find(f -> f.state == "content-going-out")
        ?? folders.find(f -> f.state == "opening" or f.state == "waiting-for-workspace")
}

/// Where a folder stands, in one sentence naming its workspace and the server keeping it.
fn folder_line(f: device.Folder) -> Text {
    let name = if f.name != "" { f.name } else { std.say("Your workspace") }
    let server = if f.server != "" { f.server } else { std.say("the server") }
    if f.state == "paused" {
        std.say("{name} is paused")
    } else if f.state == "waiting-for-workspace" {
        std.say("{name} hasn't reached this computer yet")
    } else if f.state == "opening" {
        std.say("{name} is waiting for {server}")
    } else if f.state == "stuck" {
        std.say("{name} isn't syncing: {stuck_reason(f, server)}")
    } else if f.state == "sending" {
        std.say("Sending changes in {name} to {server}")
    } else if f.state == "content-going-out" {
        std.say("Some files in {name} aren't on {server} yet")
    } else {
        std.say("{name} is in sync with {server}")
    }
}

/// Why a folder's changes wait, from the reason the folder gives, in this package's words.
fn stuck_reason(f: device.Folder, server: Text) -> Text {
    if f.stuck == "unopened" {
        std.say("{server} hasn't said which files it holds yet")
    } else if f.stuck == "unanswered" {
        std.say("{server} hasn't answered for some files yet")
    } else if f.stuck == "unarrived" {
        std.say("part of the file list hasn't arrived from {server} yet")
    } else if f.stuck == "ordered" {
        std.say("some changes wait for others to reach {server} first")
    } else if f.stuck == "refused" and f.refused == "history-full" {
        std.say("{server} has no room for more changes yet; they're kept here until it has")
    } else if f.stuck == "refused" and f.refused == "not-permitted" {
        std.say("the workspace doesn't let you make some of these changes; ask one of its owners")
    } else if f.stuck == "refused" and f.refused == "unanswered" {
        std.say("{server} isn't answering; changes are sent again shortly")
    } else if f.stuck == "refused" {
        std.say("{server} didn't take some changes; they're sent again shortly")
    } else if f.stuck == "lacking" {
        std.say("some files' details are on neither this computer nor {server}")
    } else {
        std.say("some changes haven't reached {server} yet; sending them again")
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
    let asked = asked_count()
    let label = if asked == 1 {
        "{name}, 1 question waiting"
    } else if asked > 1 {
        "{name}, {asked} questions waiting"
    } else if waiting == 0 {
        name
    } else if waiting == 1 { "{name}, 1 unread" } else { "{name}, {waiting} unread" }
    ui.Button(
        label,
        icon: if quiet { .bell_off } else { .bell },
        icon_only: true,
        role: .bar,
        count: waiting + asked,
    ) {
        on press: device.show_surface(device.overlays, surface: "centre")
    }
}
