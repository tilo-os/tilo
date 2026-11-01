/// The scrolling-strips shell: the person's desk as a two-dimensional scrolling view
/// (docs/designs/app-language/shell.md §2, §3), the way today's desk works. A *row* is a strip
/// across the desk, named, with strips stacked top to bottom and scrolled vertically; within a
/// row, each *strip* is a column of places standing side by side and scrolled horizontally; a
/// *place* is one window, showing a resource through a view.
///
/// Activating a reference in a place lands its place just after that column, so a line of work
/// stays together; activating something already shown goes to the place that shows it rather
/// than making a second one (§3). A place's content is its resource's view, drawn inside the
/// shell's frame for it: the frame is the shell's, so the content never draws its own chrome
/// (§1, §4). Every arranging change is one of the session's published actions (`arranging.tl`).
/// The places of the strip `strip`, in the order the record keeps them.
fn places_of(a: Arrangement, strip: Text) -> List<ArrangementPlace> {
    a.places.filter(p -> p.strip == strip)
}

/// The strips of the row `row`, in the record's order.
fn strips_of(a: Arrangement, row: Text) -> List<ArrangementStrip> {
    a.strips.filter(s -> s.row == row)
}

/// The rows of the workspace `workspace`, in the record's order.
fn rows_of(a: Arrangement, workspace: Text) -> List<ArrangementRow> {
    a.rows.filter(r -> r.workspace == workspace)
}

/// The preset switch: the three ways of showing the same arrangement, the one in use lit, so a
/// person moves between the strips, the framed canvas and the spaces canvas as shell.md §6 picks.
/// Switching keeps every place where it was, drawn the other way.
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
    }
}

/// The desk: its rows stacked and scrolled vertically, each row's strips scrolled sideways. One
/// two-way scroll holds the lot, so a hand can pan the whole desk as today's does.
view strips_view(a: Arrangement, v: Viewing) {
    scroll(axis: .both) {
        page(full) {
            stack(gap: .group) {
                for workspace in a.workspaces {
                    for row in rows_of(a, workspace.key) {
                        row_of(a, v, row)
                    }
                }
            }
        }
    }
}

/// One row: the strip across the desk, named, with its columns of places side by side. A narrow
/// output shows one place at a time, as the phone shell does: fewer than 30rem cannot hold two
/// columns, so the strip in front shows and the rest are a step away.
view row_of(a: Arrangement, v: Viewing, held: ArrangementRow) {
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            ui.Text(held.name, role: .subheading)
            if strips_of(a, held.key).any(s -> s.key == v.focused) {
                ui.Badge("In front", tone: .info)
            }
        }
        if space.compact {
            for s in strips_of(a, held.key).take(1) {
                strip_column(a, v, s)
            }
        } else {
            row(gap: .group, align: .start, wrap: true) {
                for s in strips_of(a, held.key) {
                    strip_column(a, v, s)
                }
                if strips_of(a, held.key).is_empty() {
                    ui.EmptyState("No window here yet", message: "Open something into this strip.", inline: true)
                }
            }
        }
    }
}

/// One strip: a column of places. The place in front shows; the others stand as tabs above it,
/// so a column of one reads as one window and a column of several as its tabs (§4). Its width,
/// what it holds and bringing it forward are the strip's own controls: a width is one
/// `resize-strip`, a place moved out one `move-place`, both attributed and undoable like a drag.
view strip_column(a: Arrangement, v: Viewing, s: ArrangementStrip) {
    let in_front = s.key == v.focused
    let placed = places_of(a, s.key)
    let chosen = placed.find(p -> p.key == s.active)
    let shown = chosen ?? placed.first()
    let current = chosen?.key ?? ""
    stack(gap: .related, min: if s.width <= 500 { 18rem } else if s.width < 900 { 30rem } else { 44rem }) {
        strip_header(a, v, s, in_front)
        if placed.len() > 1 {
            row(gap: .tight, align: .center, wrap: true) {
                for p in placed {
                    tab_chip(p, in_front and p.key == current)
                }
            }
        }
        if let p = shown {
            place_window(a, p, in_front)
        } else {
            ui.EmptyState("Nothing here", inline: true)
        }
    }
}

/// A strip's own header: a press brings it to the front, its width is one of the three, and a
/// place in it moves to another row of the desk — what a drag commits, as a press does here.
view strip_header(a: Arrangement, v: Viewing, s: ArrangementStrip, in_front: Bool) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.Text(if in_front { "In front" } else { "Bring forward" }, role: .caption, secondary: not in_front)
        if not in_front {
            ui.Button("Bring forward", icon: .chevron_right, icon_only: true, role: .plain) {
                on press: focus_strip(v, s.key)
            }
        }
        ui.MenuButton("Width", icon: .columns_2, icon_only: true, role: .plain) {
            ui.MenuItem("Narrow", checked: .some(width_of(s.width) == .narrow)) {
                on choose: resize_strip(s.key, permille_of(.narrow))
            }
            ui.MenuItem("Normal", checked: .some(width_of(s.width) == .normal)) {
                on choose: resize_strip(s.key, permille_of(.normal))
            }
            ui.MenuItem("Wide", checked: .some(width_of(s.width) == .wide)) {
                on choose: resize_strip(s.key, permille_of(.wide))
            }
        }
    }
}

/// One tab of a column: the place it stands for, lit when it is the place in front.
view tab_chip(p: ArrangementPlace, current: Bool) {
    ui.Chip(if p.title == "" { p.shows } else { p.title }, selected: .some(current))
}

/// A place's window: the shell's frame around it, and the well the app's own window shows in
/// (shell.md §8). The frame's bar is the shell's — the title, the pin and the controls that
/// close or move the place — and the window is composited under it, in the well's rectangle.
view place_window(a: Arrangement, p: ArrangementPlace, in_front: Bool) {
    ui.Card(title: if p.title == "" { p.shows } else { p.title }) {
        place_content(a, p)
    }
}

/// A place's content: its resource drawn inline through the view it names, as its own app's code
/// (shell.md §8, "The canvas composes views"), or the well the app's window shows in when there
/// is no resource here to draw (a terminal, a legacy window). The shell never draws a place's content
/// itself; the pin, close and move controls are the frame's chrome, outside it.
view place_content(a: Arrangement, p: ArrangementPlace) {
    stack(gap: .related) {
        place_controls(a, p)
        place_view(p)
    }
}

/// What stands where a place's content goes: its record drawn through the view the place names
/// (its type's default view when it names none), as its own app's code, or the well the app's
/// window shows in when the place has no record this shell can draw (a terminal, a legacy
/// window). A run of the session's agent is the shell's own resource, so its place is the
/// conversation over it (shell.md §8.1), drawn here as any other view of the shell is.
view place_view(p: ArrangementPlace) {
    if let r = placed_run(p.record) {
        conversation(r)
    } else if let r = placed_resource(p.record) {
        if p.shown_as != "" or meta(r).opens {
            show(r, view: p.shown_as)
        } else {
            ui.WindowWell(place: p.key, min: 12rem)
        }
    } else {
        ui.WindowWell(place: p.key, min: 12rem)
    }
}

/// A place's frame chrome: its pinned mark, and its pin, close and move controls. Outside the
/// well, so a press there is the shell's and one on the well is the window's.
view place_controls(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center, wrap: true) {
        if p.pinned {
            ui.Badge("Pinned", dot: true)
        }
        ui.Button("Pin", icon: .pin, icon_only: true, role: .plain) {
            on press: pin_place(p.key, not p.pinned)
        }
        ui.Button("Close place", icon: .x, icon_only: true, role: .plain) {
            on press: close_place(p.key)
        }
        if a.rows.len() > 1 {
            ui.MenuButton("Move to row", icon: .navigation, icon_only: true, role: .plain) {
                for r in a.rows {
                    if r.key != strip_row(a, p.strip) {
                        ui.MenuItem(r.name) {
                            on choose: move_place(p.key, r.key)
                        }
                    }
                }
            }
        }
    }
}

/// The row the strip `strip` stands in, by the place it holds.
fn strip_row(a: Arrangement, strip: Text) -> Text {
    match a.strips.find(s -> s.key == strip) {
        .some(s) => s.row,
        .none => "",
    }
}
