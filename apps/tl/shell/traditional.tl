/// The traditional desktop (`docs/designs/app-language/shell.md` §4, §8.2 step 5): the preset
/// furthest from the strips and the canvas. Nothing is scrolled or zoomed; every window is a
/// frame standing where the person dropped it on a floating desk, with its own bar; a top bar
/// across the screen names the focused window's commands the way macOS's action bar does; and
/// the dock rides the bottom edge (`docks.tl`, reused as it is).
///
/// Everything here is a view over resources the session publishes and the shell's own record:
///
/// - a **frame** is the shell's view of one place — its glyph, its title, its own controls, and
///   the well its window shows in (§4 `frame_classic`);
/// - the **action bar** is the focused place's resource's published actions (`actions(r)` on
///   `resource(p.record)`): the frequent ones shown directly, and the rest in the run each
///   action's own declaration names (`group:`), in the app's order. It is never a generated
///   list of everything the app can do;
/// - the **top bar** carries the system menu, the workspace, the focused window's commands, and
///   the time and status at its trailing edge, as every desktop since 1986 has.
///
/// Views name theme roles only: no colour, radius, font or size of their own.
/// The desktop's surface: each placed strip's places as frames on the floating desk, the frame
/// in front drawn last so it stands on top, exactly where the window policy put it. Each place
/// keeps its canvas point in the arrangement, so a drag that ends in one arranging action moves
/// the frame and its window together.
view traditional_desktop(a: Arrangement) {
    ui.Map(name: .some("Desktop")) {
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    if p.key != s.active {
                        desktop_frame(s, p)
                    }
                }
            }
        }
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    if p.key == s.active {
                        desktop_frame(s, p)
                    }
                }
            }
        }
    }
}

/// One frame on the desktop, at its strip's canvas point.
view desktop_frame(s: ArrangementStrip, p: ArrangementPlace) {
    ui.MapPin(name: .some(p.key), x: s.canvas_x, y: s.canvas_y, width: if s.canvas_width > 0 { s.canvas_width } else { 720 }) {
        frame_classic(s, p)
    }
}

/// A place's frame: the shell's bar over the app's window (§4). Its bar names the window by its
/// icon and title, which come from the resource and its type, never from app code, and holds
/// the shell's controls for the place at its trailing edge, outside the well so a press there
/// stays the shell's. Under the bar, the window itself shows in the well. The window's own
/// commands are not repeated here: the focused window's stand in the top bar's action strip, in
/// one place, as a macOS window's menus do.
view frame_classic(s: ArrangementStrip, p: ArrangementPlace) {
    let focused = p.key == s.active
    ui.Card(title: if p.title == "" { p.shows } else { p.title }, selected: focused) {
        stack(gap: .tight) {
            frame_bar(arrangement, p, focused)
            ui.WindowWell(place: p.key, min: 12rem)
        }
    }
}

/// A frame's bar: the app's glyph, the window's title, and the shell's controls for the place.
view frame_bar(a: Arrangement, p: ArrangementPlace, focused: Bool) {
    row(gap: .related, align: .center, fill) {
        ui.Icon(if p.record != "" { .app_window } else { .monitor }, size: .small, secondary: not focused)
        ui.Text(if p.title == "" { p.shows } else { p.title }, role: .label, lines: 1, truncate)
        spacer
        frame_buttons(a, p)
    }
}

/// A frame's controls: its pinned mark, the way to carry the place to another row, and close.
/// Each is one arranging action of the session's (`arranging.tl`), so a press, a shortcut and
/// an agent change the place the same way.
view frame_buttons(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center) {
        if p.pinned {
            ui.Badge("Pinned", dot: true)
        }
        ui.Button("Pin", icon: .pin, icon_only: true, role: .plain) {
            on press: pin_place(p.key, not p.pinned)
        }
        if a.rows.len() > 1 {
            ui.MenuButton("Move to row", icon: .navigation, icon_only: true, role: .plain) {
                for r in a.rows {
                    if r.key != p.strip {
                        ui.MenuItem(r.name) {
                            on choose: move_place(p.key, r.key)
                        }
                    }
                }
            }
        }
        ui.Button("Close place", icon: .x, icon_only: true, role: .plain) {
            on press: close_place(p.key)
        }
    }
}

/// The focused window's commands, as a macOS action bar draws them. A place showing a record
/// offers that record's own published actions — exactly what the app's window header offers —
/// grouped by the run each action's declaration names; a place that shows no record (a foreign
/// surface) offers the shell's commands on the window, so the strip is never empty.
view action_bar(a: Arrangement) {
    if let s = focused_strip(a) {
        if let p = active_place(a, s) {
            if let r = placed_resource(p.record) {
                action_run(actions(r))
            } else {
                window_commands(a, p)
            }
        } else {
            ui.Text("No window in front", role: .caption, secondary: true)
        }
    } else {
        ui.Text("No window in front", role: .caption, secondary: true)
    }
}

/// The frequent commands of `offered` as bar controls, then one menu per run the app declared,
/// in the order the app declared them, and one menu for the commands that name no run. So every
/// action the app published stays reachable, and each stands under the app's own name for it —
/// never a surface's own generated list of what did not fit.
view action_run(offered: List<host.Action>) {
    row(gap: .tight, align: .center, wrap: true) {
        for a in offered {
            if a.frequent {
                action_control(a)
            }
        }
        for a in offered {
            if starts_run(offered, a) {
                ui.MenuButton(a.group ?? "", role: .bar) {
                    for b in in_run(offered, a.group ?? "") {
                        ui.MenuItem(b.label, icon: b.icon, destructive: b.destructive, checked: b.on) {
                            on choose: b.run
                        }
                    }
                }
            }
        }
        let loose = offered.filter(a -> not a.frequent and a.group == .none)
        if not loose.is_empty() {
            ui.MenuButton("Window", role: .bar) {
                for a in loose {
                    ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive, checked: a.on) {
                        on choose: a.run
                    }
                }
            }
        }
    }
}

/// One command the person reaches for often: a bar control, lifted while the state it toggles
/// is on.
view action_control(a: host.Action) {
    if a.on != .none {
        ui.Button(a.label, icon: a.icon, toggled: a.on, role: .bar) {
            on press: a.run
        }
    } else {
        ui.Button(a.label, icon: a.icon, role: .bar) {
            on press: a.run
        }
    }
}

/// Whether `a` is the first command of its run: the run's menu is drawn once, at the place the
/// app declared its first command, so the menus come in the app's own order.
fn starts_run(offered: List<host.Action>, a: host.Action) -> Bool {
    if a.group == .none { false } else if let first = offered.find(b -> (b.group ?? "") == (a.group ?? "")) { first.label == a.label } else { false }
}

/// The commands of `offered` that stand in the run `run`, in the app's order.
fn in_run(offered: List<host.Action>, run: Text) -> List<host.Action> {
    offered.filter(a -> (a.group ?? "") == run)
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

/// The record a place shows, as its own type (`resource(id)`), so this view reads the same
/// actions the app's window header does. `.none` for a place showing no record this store
/// holds.
fn placed_resource(record: Text) -> Option<host.Resource> {
    if record == "" { .none } else { resource(record) }
}

/// The shell's own commands on a window whose record it cannot read (a foreign surface): the
/// place in front, moving it to another row, and closing it.
view window_commands(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.Button("Pin", icon: .pin, icon_only: true, role: .bar) {
            on press: pin_place(p.key, not p.pinned)
        }
        if a.rows.len() > 1 {
            ui.MenuButton("Window", role: .bar) {
                for r in a.rows {
                    if r.key != p.strip {
                        ui.MenuItem(r.name, icon: .navigation) {
                            on choose: move_place(p.key, r.key)
                        }
                    }
                }
            }
        }
        ui.Button("Close", icon: .x, role: .destructive) {
            on press: close_place(p.key)
        }
    }
}

/// The focused window's commands as a strip of their own under the bar, on a narrow output
/// where they do not fit beside the bar's own parts: the same strip, drawn with the window's
/// name leading it so it reads as the window's, wrapping onto another line when it needs to.
view traditional_strip(a: Arrangement) {
    let name = if let s = focused_strip(a) {
        if let p = active_place(a, s) {
            if p.title == "" { p.shows } else { p.title }
        } else { "Window" }
    } else { "Window" }
    stack(gap: .tight) {
        ui.Text(name, role: .caption, secondary: true)
        action_bar(a)
    }
}

/// Who is here, on the bar: a chip per person (each opens their menu) and the person's own
/// avatar. The avatar alone carries who this device is for a bar that has room for it; the
/// chips carry who else is, and each is the way to their menu.
view people_here(d: Desk) {
    row(gap: .tight, align: .center, wrap: true) {
        for p in d.people {
            person_chip(p)
        }
        if d.person != "" {
            ui.Avatar(name: .some(d.person))
        }
    }
}

/// The desktop's top bar: the system menu and the workspace leading, the focused window's
/// commands beside them, and the time and the status trailing — the shape a macOS or GNOME bar
/// has, over the theme's bar material. A narrow output keeps the window's commands on a line of
/// their own under the bar's own (`traditional_strip`), where they wrap rather than press into
/// what is beside them, as the shell's bar parts give way at `space.compact` (`app.tl`).
view traditional_bar(d: Desk) {
    let roomy = space.width_class == .expanded or space.width_class == .wide
    let packed = space.compact or space.width_class == .medium
    stack(gap: .tight) {
        row(gap: .related, align: .center, fill) {
            ui.Button("Tilo", icon: .tilo, icon_only: true, role: .bar) {
                on press: system_menu(d)
            }
            workspace_menu(d)
            if roomy {
                action_bar(arrangement)
            }
            spacer
            if roomy and d.tasks.any(t -> t.state == "running") {
                ui.Button("Working", icon: .bot, role: .bar) {
                    on press: ask(d)
                }
            }
            clock(d)
            if roomy and (not d.people.is_empty() or d.person != "") {
                people_here(d)
            }
            bell(d)
        }
        if packed {
            traditional_strip(arrangement)
        }
    }
}
