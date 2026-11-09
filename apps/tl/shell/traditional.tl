/// The traditional desktop (`docs/designs/app-language/shell.md` §4, §8.2 step 5): the preset
/// furthest from the strips and the canvas. Nothing is scrolled or zoomed; every window is a
/// frame standing where the person dropped it on a floating desk, with its own bar; a top bar
/// across the screen names the focused window and holds its commands the way macOS's action bar
/// does; and the dock rides the bottom edge (`docks.tl`, reused as it is).
///
/// Everything here is a view over resources the session publishes and the shell's own record:
///
/// - a **frame** is the shell's view of one place: the window — the resource drawn through its
///   type's own view (`show(r)`, §8), its header holding its title and actions, or a well a
///   foreign window shows in (§4 `frame_classic`) — with the shell's controls at the end of its
///   one bar;
/// - the **action bar** is the focused place's window's menu: the host fills it with every
///   action `resource(p.record)` publishes, so the window in front keeps one list of what it can
///   do now, in the app's own words;
/// - the **top bar** is one row, edge to edge: the system menu and the focused window's
///   commands leading, the tasks, the time, who is here, the status, the notifications and the
///   person's own face flush at its trailing edge, as every desktop since 1986 has. The
///   shell's own settings are never there:
///   they are in Settings and the system menu.
///
/// Views name theme roles only: no colour, radius, font or size of their own.
/// The desktop's surface: each placed strip's places as frames on the floating desk, over the
/// desktop's ground, the frame in front drawn last so it stands on top, exactly where the
/// window policy put it. Each place keeps its canvas point in the arrangement, so a drag that
/// ends in one arranging action moves the frame and its window together. `lists` says whether
/// the desk's bar lists the window in front's actions (`bar_lists_actions`), as each frame is
/// told: the window's bar then keeps its title and the shell's controls alone.
view traditional_desktop(a: Arrangement, lists: Bool) {
    let front = front_place(a)
    ui.Map(name: "Desktop", ground: true, fill) {
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
view desktop_frame(
    a: Arrangement,
    s: ArrangementStrip,
    p: ArrangementPlace,
    focused: Bool,
    lists: Bool,
) {
    ui.MapPin(
        name: p.key,
        x: s.canvas_x,
        y: s.canvas_y,
        width: if s.canvas_width > 0 { s.canvas_width } else { 720 },
        height: if s.canvas_height > 0 { s.canvas_height } else { 480 },
    ) {
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
/// the frame tells the window's bar to keep its title and the shell's controls alone, the actions
/// standing once in the desk's bar's menu.
view frame_classic(a: Arrangement, p: ArrangementPlace, focused: Bool, lists: Bool) {
    ui.WindowFrame(name: place_title(p), focused: focused, lists_actions: lists, fill) {
        controls: {
            frame_buttons(a, p)
        }
        body: {
            place_view(p, fill)
        }
    }
}

/// What a place is called, on every surface: `p.name`, resolved once by the session for all of
/// them — the window's title and its app's own name (`Groceries — Notes`), or the app alone when
/// the window has no title. A bar, a chip, a switcher row and a search row never disagree, and
/// none of them ever falls back to an app id.
fn place_title(p: ArrangementPlace) -> Text {
    if p.name != "" {
        p.name
    } else if p.title != "" { p.title } else if p.shows != "" { p.shows } else { "Window" }
}

/// A frame's controls: the way to carry the place to another row, and close. Each is one
/// arranging action of the session's (`shell:arrangement`), so a press, a shortcut and an agent
/// change the place the same way.
view frame_buttons(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center) {
        if a.rows.len() > 1 {
            ui.MenuButton("Move to row", icon: .navigation, icon_only: true, role: .bar) {
                for r in a.rows {
                    if r.key != strip_row(a, p.strip) {
                        ui.MenuItem(r.name) {
                            on choose: session.move(place: the_place(p.key), row: the_row(r.key))
                        }
                    }
                }
            }
        }
        ui.Button("Close", icon: .x, icon_only: true, role: .bar) {
            on press: session.close(place: the_place(p.key))
        }
    }
}

/// The focused window's commands, as a macOS menu bar draws an app's: the window's name, and
/// under it every action its resource publishes — exactly what the window's own heading would
/// offer, in the app's own words, the host filling the menu from the declarations so a view
/// never lists them itself. A place that shows no record (a foreign surface) offers the shell's
/// commands on the window instead, so the bar is never empty. `room` is how much the top bar has:
/// on the narrowest it stands as the window's icon, named by the window.
view action_bar(a: Arrangement, room: BarRoom) {
    if let s = focused_strip(a) {
        if let p = active_place(a, s) {
            if let r = placed_resource(p.record) {
                ui.MenuButton(
                    place_title(p),
                    icon: .app_window,
                    icon_only: room == .one,
                    role: .bar,
                    subject: r,
                )
            } else {
                window_commands(a, p, room)
            }
        }
    }
}

/// How much room the top bar has: for the focused window's menu, and for its parts.
enum BarRoom {
    /// The most: the menu by its window's name, the tasks and who is here too.
    roomy
    /// The menu by its window's name, but not the tasks and who is here.
    menus
    /// The least: the menu stands as the window's icon, and only what a phone keeps stays.
    one
}

/// The strip in front: the one the shell's own view of the arrangement names, else the first
/// that has a place on the desktop.
fn focused_strip(a: Arrangement) -> Option<ArrangementStrip> {
    let v = viewing
    if let named = a.strips.find(s -> s.key == v.focused) {
        named
    } else { a.strips.find(s -> s.placed) }
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
    if record != "" { resource(record) }
}

/// The shell's own commands on a window whose record it cannot read (a foreign surface): its
/// name, moving it to another row, and closing it. On the narrowest bar the menu is named by the
/// window's icon alone, as an app's window's one menu is.
view window_commands(a: Arrangement, p: ArrangementPlace, room: BarRoom) {
    ui.MenuButton(place_title(p), icon: .app_window, icon_only: room == .one, role: .bar) {
        for r in a.rows {
            if r.key != strip_row(a, p.strip) {
                ui.MenuItem(r.name, icon: .navigation) {
                    on choose: session.move(place: the_place(p.key), row: the_row(r.key))
                }
            }
        }
        ui.MenuItem("Close", icon: .x) {
            on choose: session.close(place: the_place(p.key))
        }
    }
}

/// The desktop's top bar, one row edge to edge: the system menu and the focused window's menu
/// leading, and the tasks running, the time, who is here, the status, the notifications and the
/// person's own face flush at the trailing edge, as a macOS or GNOME bar has them. As the output
/// narrows the window's menu stands as its icon, the tasks and who is here give way, and on a
/// phone the sound does too (the time, the connection, the charge, the notifications and the
/// person's face stay), so the row never wraps. Its controls stand a cluster gap apart at every
/// width, a finger's or a mouse's press target being wider than the face it is drawn in: at the
/// tightest gap a press between two would reach whichever is drawn last.
view traditional_bar(d: Desk) {
    let room = if space.width_class == .wide {
        BarRoom.roomy
    } else if space.compact { BarRoom.one } else { BarRoom.menus }
    row(gap: .related, align: .center, fill) {
        ui.Button("Tilo", icon: .tilo, icon_only: true, role: .bar) {
            on press: session.tilo_menu()
        }
        action_bar(arrangement, room)
        spacer
        clock(d)
        if room != .one {
            people_button(d)
        }
        if not space.compact {
            volume_status()
        }
        network_status()
        battery_status()
        bell(d)
        if d.person != "" {
            ui.Avatar(name: d.person)
        }
    }
}
