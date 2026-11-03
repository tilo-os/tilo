/// The strips shell: the person's desk as a desk of windows (docs/designs/app-language/shell.md
/// §2, §3). A *row* is a band across the desk holding *strips*, a *strip* is a column of
/// *places*, and a *place* is one window. A window is a window: its strip fills the room the row
/// gives it, its bar runs across its top edge, and its content fills everything under that one
/// bar. A strip of several places shows them as its tabs, the place in front filling the window;
/// a strip of one place is a window of one, its bar the place's own header — or, for a place
/// whose app draws none (a terminal, a foreign window), the frame's bar naming it, and on a
/// window nobody is in, that window's decorations alone.
///
/// The frame is `ui.WindowFrame`: one window bar for the whole shell, with the window's own
/// controls in a pill of the floating material at its trailing end, so a person sees the same
/// bar here as on the frames, the spaces and the traditional desktop. Every arranging change is
/// one of the session's published actions (`shell:arrangement`): a press, a tab, a script and an
/// agent arrange the same way, attributed and undoable.
///
/// Rows of several strips stand side by side across the row's width, each strip taking the share
/// its own width names; a row whose strips ask for more than the desk holds scrolls sideways
/// rather than squeezing a window under its share, and a narrow output stacks them instead, one
/// window under the other, since a phone cannot hold two windows beside each other.
/// The places of the strip `strip`, in the order the record keeps them.
fn places_of(a: Arrangement, strip: Text) -> List<ArrangementPlace> {
    a.places.filter(p -> p.strip == strip)
}

/// The strips the row `row` holds, in the record's order.
fn strips_of(a: Arrangement, row: Text) -> List<ArrangementStrip> {
    a.strips.filter(s -> s.row == row)
}

/// The rows of the workspace `workspace`, in the record's order.
fn rows_of(a: Arrangement, workspace: Text) -> List<ArrangementRow> {
    a.rows.filter(r -> r.workspace == workspace)
}

/// The strip the shell shows in front: the one the person brought forward, or the first of the
/// desk while they have brought none forward yet.
fn strip_in_front(a: Arrangement, v: Viewing) -> Text {
    if v.focused != "" { v.focused } else { a.strips.first()?.key ?? "" }
}

/// The strips standing maximized: each shows alone as the window the desk is, the others behind
/// it. Empty while none is.
fn maximized_strips(a: Arrangement) -> List<ArrangementStrip> {
    a.strips.filter(s -> s.maximized)
}

/// The preset switch: the ways of showing the same arrangement, the one in use lit, so a person
/// moves between the strips, the framed canvas, the spaces canvas and the traditional desktop as
/// shell.md §6 picks. Switching keeps every place where it was, drawn the other way. The desk
/// carries it in its own bar (`preset_menu`); this is the page Settings shows.
view preset_switch(v: Viewing) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.Chip("Strips", icon: .columns_2, selected: .some(v.showing == .strips)) {
            on press: show_preset(v, .strips)
        }
        ui.Chip("Frames", icon: .app_window, selected: .some(v.showing == .frames)) {
            on press: show_preset(v, .frames)
        }
        ui.Chip("Spaces", icon: .layout_grid, selected: .some(v.showing == .spaces)) {
            on press: show_preset(v, .spaces)
        }
        ui.Chip("Desktop", icon: .monitor, selected: .some(v.showing == .desktop)) {
            on press: show_preset(v, .desktop)
        }
    }
}

/// The preset the desk shows, named the way the person picked it.
fn preset_title(p: Preset) -> Text {
    match p {
        .strips => "Strips",
        .frames => "Frames",
        .spaces => "Spaces",
        .desktop => "Desktop",
    }
}

/// The preset the desk shows as a menu of the bar's own: the same switch the Settings page
/// offers, where the person passes it on the desk, so the desk's windows start at the bar's edge
/// and no second row stands between them and it.
view preset_menu(v: Viewing) {
    ui.MenuButton(preset_title(v.showing), icon: .layout_grid, role: .bar) {
        ui.MenuItem("Strips", icon: .columns_2, checked: .some(v.showing == .strips)) {
            on choose: show_preset(v, .strips)
        }
        ui.MenuItem("Frames", icon: .app_window, checked: .some(v.showing == .frames)) {
            on choose: show_preset(v, .frames)
        }
        ui.MenuItem("Spaces", icon: .layout_grid, checked: .some(v.showing == .spaces)) {
            on choose: show_preset(v, .spaces)
        }
        ui.MenuItem("Desktop", icon: .monitor, checked: .some(v.showing == .desktop)) {
            on choose: show_preset(v, .desktop)
        }
    }
}

/// The desk's windows, filling the room between the bar and the dock: the maximized strips alone
/// when one is, else each row of the workspace in front with its strips side by side.
view strips_view(a: Arrangement, v: Viewing, lists: Bool) {
    let maximizing = maximized_strips(a)
    if not maximizing.is_empty() {
        row(gap: .group, fill) {
            for s in maximizing {
                strip_window(a, v, s, lists, fill)
            }
        }
    } else {
        scroll(axis: .both) {
            page(full) {
                stack(gap: .group, fill) {
                    for workspace in a.workspaces {
                        for row in rows_of(a, workspace.key) {
                            row_of(a, v, row, lists, fill)
                        }
                    }
                }
            }
        }
    }
}

/// One row: the band across the desk, named while the desk holds more than one so a person can
/// tell them apart, with its strips side by side across the room the row is given, each taking
/// the share its own width names. A phone's or a tablet's output cannot hold a window beside
/// another, so there each strip stands under the last, a window of its own height, in the page.
view row_of(a: Arrangement, v: Viewing, held: ArrangementRow, lists: Bool) {
    let roomy = space.width_class == .expanded or space.width_class == .wide
    let strips = strips_of(a, held.key)
    stack(gap: .related, fill) {
        if a.rows.len() > 1 {
            ui.Text(held.name, role: .subheading)
        }
        if not roomy {
            stack(gap: .group) {
                for s in strips {
                    strip_window(a, v, s, lists, aspect: 4/3)
                }
                if strips.is_empty() {
                    ui.EmptyState("No window here yet", message: "Open something into this strip.", inline: true)
                }
            }
        } else {
            row(gap: .group, fill) {
                for s in strips {
                    // Its share of the row, never under what its own width names: a window
                    // narrower than its share is worse than the row scrolling to it whole.
                    strip_window(a, v, s, lists, fill(if s.width > 0 { s.width } else { 667 }), min: if s.width <= 500 { 26rem } else if s.width < 900 { 51rem } else { 76rem })
                }
                if strips.is_empty() {
                    ui.EmptyState("No window here yet", message: "Open something into this strip.", inline: true)
                }
            }
        }
    }
}

/// One strip: the window its places make, filling the room it is given. Its bar runs across its
/// top edge — the tabs of its places while it shows several, the place's own header while it
/// shows one — and the place in front fills everything under it. A strip holding nothing is an
/// empty window, saying so.
view strip_window(a: Arrangement, v: Viewing, s: ArrangementStrip, lists: Bool) {
    let in_front = s.key == strip_in_front(a, v)
    let placed = places_of(a, s.key)
    let chosen = placed.find(p -> p.key == s.active)
    let shown = chosen ?? placed.first()
    ui.WindowFrame(name: .some(window_name(shown)), focused: in_front, lists_actions: lists) {
        controls: {
            window_controls(a, v, s, shown, in_front)
        }
        tabs: {
            if placed.len() > 1 {
                ui.TabStrip(place_in_front(s), focused: in_front) {
                    on activate(p: session.Place): show_place(v, s.key, p)
                    on close(p: session.Place): session.close(place: p)
                    for p in placed {
                        ui.TabItem(the_place(p.key), title: .some(place_title(p)))
                    }
                }
            }
        }
        body: {
            if let p = shown {
                place_content(p)
            } else {
                ui.EmptyState("Nothing here", message: "Open something into this strip.", inline: true)
            }
        }
    }
}

/// What a strip as a whole is called: the place in front, or an empty strip.
fn window_name(shown: Option<ArrangementPlace>) -> Text {
    if let p = shown { place_title(p) } else { "Empty strip" }
}

/// The resource of the place in front of `s`, as its tab names it: empty while the strip has no
/// place in front, which lights its first tab instead.
fn place_in_front(s: ArrangementStrip) -> Option<session.Place> {
    if s.active != "" { .some(the_place(s.active)) } else { .none }
}

/// Shows the place `place` and brings its strip forward with it: what choosing a tab does, and
/// the same arranging action a script or an agent runs.
action show_place(v: Viewing, strip: Text, place: session.Place) (icon: .app_window, label: "Show the place") {
    v.focused = strip
    session.activate(place: place)
}

/// A window's own controls, in the pill at the trailing end of its one bar: the strip's width,
/// the window's place in front, maximize or restore, and the place's pin, move and close. Each
/// is one arranging action of the session's, so a press and an agent change the desk the same
/// way, and none of them ever asks for a bar of its own.
view window_controls(a: Arrangement, v: Viewing, s: ArrangementStrip, shown: Option<ArrangementPlace>, in_front: Bool) {
    row(gap: .tight, align: .center) {
        ui.MenuButton("Width", icon: .columns_2, icon_only: true, role: .plain) {
            ui.MenuItem("Narrow", checked: .some(width_of(s.width) == .narrow)) {
                on choose: session.resize(strip: the_strip(s.key), permille: permille_of(.narrow))
            }
            ui.MenuItem("Normal", checked: .some(width_of(s.width) == .normal)) {
                on choose: session.resize(strip: the_strip(s.key), permille: permille_of(.normal))
            }
            ui.MenuItem("Wide", checked: .some(width_of(s.width) == .wide)) {
                on choose: session.resize(strip: the_strip(s.key), permille: permille_of(.wide))
            }
        }
        if not in_front {
            ui.Button("Bring to front", icon: .app_window, icon_only: true, role: .plain) {
                on press: focus_strip(v, s.key)
            }
        }
        if s.maximized {
            ui.Button("Restore", icon: .minimize_2, icon_only: true, role: .plain) {
                on press: session.maximize(strip: the_strip(s.key), maximized: false)
            }
        } else {
            ui.Button("Maximize", icon: .maximize_2, icon_only: true, role: .plain) {
                on press: session.maximize(strip: the_strip(s.key), maximized: true)
            }
        }
        if let p = shown {
            place_controls(a, p)
        }
    }
}

/// A place's own controls: its pinned mark, its pin, its move to another row, and its close.
/// Outside the well, so a press there is the shell's and one on the well is the app's.
view place_controls(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center) {
        if p.pinned {
            ui.Button("Pinned", icon: .pin, icon_only: true, toggled: .some(true), role: .plain) {
                on press: session.pin(place: the_place(p.key), pinned: false)
            }
        } else {
            ui.Button("Pin", icon: .pin, icon_only: true, role: .plain) {
                on press: session.pin(place: the_place(p.key), pinned: true)
            }
        }
        if a.rows.len() > 1 {
            ui.MenuButton("Move to row", icon: .navigation, icon_only: true, role: .plain) {
                for r in a.rows {
                    if r.key != strip_row(a, p.strip) {
                        ui.MenuItem(r.name) {
                            on choose: session.move(place: the_place(p.key), row: the_row(r.key))
                        }
                    }
                }
            }
        }
        ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
            on press: session.close(place: the_place(p.key))
        }
    }
}

/// A place's body: its resource drawn inline through the view it names, as its own app's code
/// (shell.md §8, "The canvas composes views"), or the well the app's window shows in when there
/// is no resource here to draw (a terminal, a legacy window), filling the room it is given. The
/// shell never draws a place's content itself; the pin, close and move controls are the frame's
/// chrome, outside it (`place_controls`).
///
/// Every preset draws a place through this one view — a strip's body, a framed canvas pin, a
/// space on the frameless canvas — so a place looks the same whichever preset shows it.
view place_content(p: ArrangementPlace) {
    place_view(p, fill)
}

/// A place's content: its resource drawn inline through the view it names, as its own app's code
/// (shell.md §8, "The canvas composes views"), or the well the app's window shows in when there
/// is no resource here to draw (a terminal, a legacy window). A run of the session's agent is the
/// shell's own resource, so its place is the conversation over it (shell.md §8.1), drawn here as
/// any other view of the shell is; the window's controls are the frame's, in its one bar, outside
/// it.
view place_view(p: ArrangementPlace) {
    if let r = placed_run(p.record) {
        conversation(r, fill)
    } else if let r = placed_resource(p.record) {
        if p.shown_as != "" or meta(r).opens {
            show(r, view: p.shown_as, fill)
        } else {
            ui.WindowWell(place: p.key, fill)
        }
    } else {
        ui.WindowWell(place: p.key, fill)
    }
}

/// The row the strip `strip` stands in, by the place it holds.
fn strip_row(a: Arrangement, strip: Text) -> Text {
    match a.strips.find(s -> s.key == strip) {
        .some(s) => s.row,
        .none => "",
    }
}
