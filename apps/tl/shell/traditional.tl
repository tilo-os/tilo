/// The traditional desktop (`docs/designs/app-language/shell.md` §4, §8.2 step 5): the preset
/// furthest from the strips and the canvas. Nothing is scrolled or zoomed; every window is a
/// frame standing where the person dropped it on a floating desk, with its own bar; a top bar
/// across the screen names the focused window's commands the way macOS's action bar does; and
/// the dock rides the bottom edge (`docks.tl`, reused as it is).
///
/// Everything here is a view over resources the session publishes and the shell's own record:
///
/// - a **frame** is the shell's view of one place: the window — the resource drawn through its
///   type's own view (`show(r)`, §8), its header holding its title and actions, or a well a
///   foreign window shows in (§4 `frame_classic`) — with the shell's controls at the end of its
///   one bar;
/// - the **action bar** is the focused place's resource's published actions (`actions(r)` on
///   `resource(p.record)`): the frequent ones shown directly, and the rest in the run each
///   action's own declaration names (`group:`), in the app's order. It is never a generated
///   list of everything the app can do;
/// - the **top bar** is one row, edge to edge: the system menu and the focused window's
///   commands leading, the tasks, the time, the person and the notifications flush at its
///   trailing edge, as every desktop since 1986 has. The shell's own settings are never there:
///   they are in Settings and the system menu.
///
/// Views name theme roles only: no colour, radius, font or size of their own.
/// The desktop's surface: each placed strip's places as frames on the floating desk, over the
/// desktop's ground, the frame in front drawn last so it stands on top, exactly where the
/// window policy put it. Each place keeps its canvas point in the arrangement, so a drag that
/// ends in one arranging action moves the frame and its window together. `lists` says whether
/// the desk's bar lists the window in front's actions (`bar_lists_actions`), as each frame is
/// told.
view traditional_desktop(a: Arrangement, lists: Bool) {
    let front = front_place(a)
    ui.Map(name: .some("Desktop"), ground: true, fill) {
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    if p.key != front {
                        desktop_frame(a, s, p, false, lists)
                    }
                }
            }
        }
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    if p.key == front {
                        desktop_frame(a, s, p, true, lists)
                    }
                }
            }
        }
    }
}

/// The window in front alone, filling the room it is given: what a compact output shows of the
/// desktop (`desk`), where windows standing where they were left would show their corners cut
/// off and the window the person is in could stand behind another.
view front_window(a: Arrangement, lists: Bool) {
    if let s = focused_strip(a) {
        if let p = active_place(a, s) {
            frame_classic(a, p, true, lists, fill)
        }
    }
}

/// One frame on the desktop, at its strip's canvas point and size.
view desktop_frame(a: Arrangement, s: ArrangementStrip, p: ArrangementPlace, focused: Bool, lists: Bool) {
    ui.MapPin(name: .some(p.key), x: s.canvas_x, y: s.canvas_y, width: if s.canvas_width > 0 { s.canvas_width } else { 720 }, height: if s.canvas_height > 0 { s.canvas_height } else { 480 }) {
        frame_classic(a, p, focused, lists)
    }
}

/// A place's frame (§4): the window, with the shell's controls for the place at the trailing
/// end of its bar. A resource stands drawn through its type's own view, as its app draws it
/// (`show`): its header names it by its title, which comes from the resource and its type, and
/// holds its own actions, and the frame puts its controls at that header's end, so a window has
/// one bar, never the shell's stacked on the app's. A foreign window stands in its well, under
/// the frame's own bar naming the place. The controls stay the shell's, so a press there never
/// reaches the app. `lists`: the desk's bar lists the window's actions (`bar_lists_actions`), so
/// the frame tells its header to let what does not fit give way to that menu instead of a door.
view frame_classic(a: Arrangement, p: ArrangementPlace, focused: Bool, lists: Bool) {
    ui.WindowFrame(name: .some(place_title(p)), focused: focused, lists_actions: lists, fill) {
        controls: {
            frame_buttons(a, p)
        }
        body: {
            place_view(p, fill)
        }
    }
}

/// What a place is called: its resource's title, else the title the place was given, else what
/// it shows.
fn place_title(p: ArrangementPlace) -> Text {
    if let r = placed_resource(p.record) { meta(r).title } else if p.title == "" { p.shows } else { p.title }
}

/// A frame's controls: the way to carry the place to another row, and close. Each is one
/// arranging action of the session's (`shell:arrangement`), so a press, a shortcut and an agent
/// change the place the same way.
view frame_buttons(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center) {
        if a.rows.len() > 1 {
            ui.MenuButton("Move to row", icon: .navigation, icon_only: true, role: .bar) {
                for r in a.rows {
                    if r.key != p.strip {
                        ui.MenuItem(r.name) {
                            on choose: session.move_place(place: arranged(p.key), row: arranged(r.key))
                        }
                    }
                }
            }
        }
        ui.Button("Close", icon: .x, icon_only: true, role: .bar) {
            on press: session.close_place(place: arranged(p.key))
        }
    }
}

/// The focused window's commands, as a macOS menu bar draws an app's: the window's name, then
/// the place's resource's own published actions — exactly what the app's window header offers
/// — the frequent ones as controls and the rest in the run each action's declaration names. A
/// place that shows no record (a foreign surface) offers the shell's commands on the window,
/// so the strip is never empty. `room` is how much the bar has: on a narrow bar the frequent
/// ones stand only in their runs' menus, and on the narrowest every run is one menu named by
/// the window, so the bar sheds into menus and never wraps.
view action_bar(a: Arrangement, room: BarRoom) {
    if let s = focused_strip(a) {
        if let p = active_place(a, s) {
            if let r = placed_resource(p.record) {
                action_run(place_title(p), actions(r), room)
            } else {
                window_commands(a, p, room)
            }
        }
    }
}

/// How much room the top bar has for the focused window's commands.
enum BarRoom {
    /// The frequent commands as controls, every run as a menu.
    roomy
    /// Every run as a menu; the frequent commands within them.
    menus
    /// One menu, named by the window, with every run in it.
    one
}

/// The window's name, then the frequent commands of `offered` as bar controls, then one menu
/// per run the app declared, in the order the app declared them, and the commands that name no
/// run under the window's own name. So every action the app published stays reachable, and
/// each stands under the app's own name for it — never a surface's own generated list of what
/// did not fit. Below the room the bar's fixed parts take (the system menu, the time, the
/// notifications), the one menu stands as the window's icon, named by the window.
view action_run(name: Text, offered: List<host.Action>, room: BarRoom) {
    let loose = offered.filter(c -> c.group == .none and (room != .roomy or not c.frequent))
    row(gap: .tight, align: .center) {
        if room == .one {
            ui.MenuButton(name, icon: .app_window, icon_only: space.width < 18.0, role: .bar) {
                for c in offered {
                    ui.MenuItem(c.label, icon: c.icon, destructive: c.destructive, checked: c.on) {
                        on choose: c.run
                    }
                }
            }
        } else {
            if loose.is_empty() {
                ui.Text(name, role: .label, lines: 1, truncate)
            } else {
                ui.MenuButton(name, role: .bar) {
                    for c in loose {
                        ui.MenuItem(c.label, icon: c.icon, destructive: c.destructive, checked: c.on) {
                            on choose: c.run
                        }
                    }
                }
            }
            if room == .roomy {
                for c in offered {
                    if c.frequent {
                        action_control(c)
                    }
                }
            }
            for c in offered {
                if starts_run(offered, c) {
                    ui.MenuButton(c.group ?? "", role: .bar) {
                        for b in in_run(offered, c.group ?? "") {
                            ui.MenuItem(b.label, icon: b.icon, destructive: b.destructive, checked: b.on) {
                                on choose: b.run
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One command the person reaches for often: a bar control, lifted while the state it toggles
/// is on.
view action_control(c: host.Action) {
    if c.on != .none {
        ui.Button(c.label, icon: c.icon, toggled: c.on, role: .bar) {
            on press: c.run
        }
    } else {
        ui.Button(c.label, icon: c.icon, role: .bar) {
            on press: c.run
        }
    }
}

/// Whether `c` is the first command of its run: the run's menu is drawn once, at the place the
/// app declared its first command, so the menus come in the app's own order.
fn starts_run(offered: List<host.Action>, c: host.Action) -> Bool {
    if c.group == .none { false } else if let first = offered.find(b -> (b.group ?? "") == (c.group ?? "")) { first.label == c.label } else { false }
}

/// The commands of `offered` that stand in the run `run`, in the app's order.
fn in_run(offered: List<host.Action>, run: Text) -> List<host.Action> {
    offered.filter(c -> (c.group ?? "") == run)
}

/// The strip in front: the one the shell's own view of the arrangement names, else the first
/// that has a place on the desktop.
fn focused_strip(a: Arrangement) -> Option<ArrangementStrip> {
    let v = viewing
    if let named = a.strips.find(s -> s.key == v.focused) { named } else { a.strips.find(s -> s.placed) }
}

/// The place in front of `s`: its active place, else the first one it holds.
fn active_place(a: Arrangement, s: ArrangementStrip) -> Option<ArrangementPlace> {
    let held = places_of(a, s.key)
    if let chosen = held.find(p -> p.key == s.active) { chosen } else { held.first() }
}

/// The key of the window in front: the focused strip's place in front; empty for none.
fn front_place(a: Arrangement) -> Text {
    if let s = focused_strip(a) {
        if let p = active_place(a, s) {
            return p.key
        }
    }
    ""
}

/// The record a place shows, as its own type (`resource(id)`), so this view reads the same
/// actions the app's window header does. `.none` for a place showing no record this store
/// holds.
fn placed_resource(record: Text) -> Option<host.Resource> {
    if record == "" { .none } else { resource(record) }
}

/// The shell's own commands on a window whose record it cannot read (a foreign surface): its
/// name, moving it to another row, and closing it. On the narrowest bar the menu is named by the
/// window's icon alone, as an app's window's one menu is.
view window_commands(a: Arrangement, p: ArrangementPlace, room: BarRoom) {
    ui.MenuButton(place_title(p), icon: .app_window, icon_only: room == .one and space.width < 18.0, role: .bar) {
        for r in a.rows {
            if r.key != p.strip {
                ui.MenuItem(r.name, icon: .navigation) {
                    on choose: session.move_place(place: arranged(p.key), row: arranged(r.key))
                }
            }
        }
        ui.MenuItem("Close", icon: .x) {
            on choose: session.close_place(place: arranged(p.key))
        }
    }
}

/// The desktop's top bar, one row edge to edge: the system menu and the focused window's
/// commands leading; the tasks running, the time, the person and the notifications flush at the
/// trailing edge — the shape a macOS or GNOME bar has, over the theme's bar material. As the
/// output narrows the window's commands shed into their menus, the tasks give way, and on a
/// phone the person's avatar does too (the time and the notifications stay), so the row never
/// wraps and its parts never press into each other.
view traditional_bar(d: Desk) {
    let room = if space.width_class == .wide { BarRoom.roomy } else if space.compact { BarRoom.one } else { BarRoom.menus }
    row(gap: if room == .one { .tight } else { .related }, align: .center, fill) {
        ui.Button("Tilo", icon: .tilo, icon_only: true, role: .bar) {
            on press: session.toggle_tilo_menu()
        }
        action_bar(arrangement, room)
        spacer
        if room == .roomy {
            for t in d.tasks {
                if t.state == "running" {
                    ui.Button(t.title, icon: .bot, role: .bar) {
                        on press: device.show_surface(device.desk, surface: "composer")
                    }
                }
            }
        }
        clock(d)
        if d.person != "" and room != .one {
            ui.Avatar(name: .some(d.person))
        }
        bell(d)
    }
}
