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
/// (§1, §4). Every arranging change is one of the session's published actions (`shell:arrangement`).
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

/// The preset switch: the four ways of showing the same arrangement, the one in use lit, so a
/// person moves between the strips, the framed canvas, the spaces canvas and the traditional
/// desktop as shell.md §6 picks. Switching keeps every place where it was, drawn the other way.
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

/// One row: the strip across the desk, named, with its columns of places side by side. A phone's
/// or a tablet's output cannot hold a wide column beside another, so there each strip takes the
/// output's width, one under the other, as the phone shell shows them.
view row_of(a: Arrangement, v: Viewing, held: ArrangementRow) {
    stack(gap: .related) {
        ui.Text(held.name, role: .subheading)
        if not (space.width_class == .expanded or space.width_class == .wide) {
            stack(gap: .group) {
                for s in strips_of(a, held.key) {
                    strip_column(a, v, s)
                }
                if strips_of(a, held.key).is_empty() {
                    ui.EmptyState("No window here yet", message: "Open something into this strip.", inline: true)
                }
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

/// One strip: a column of places in the shell's frame, headed by the strip's name (what its
/// place in front shows) and lit while it is the strip in front; a press on it brings it to the
/// front. The place in front shows; the others stand as tabs above it, so a column of one reads
/// as one window and a column of several as its tabs (§4).
view strip_column(a: Arrangement, v: Viewing, s: ArrangementStrip) {
    let in_front = s.key == v.focused
    let placed = places_of(a, s.key)
    let chosen = placed.find(p -> p.key == s.active)
    let shown = chosen ?? placed.first()
    let current = chosen?.key ?? ""
    let name = match shown {
        .some(p) => place_title(p),
        .none => "Empty strip",
    }
    ui.Card(title: name, selected: in_front, min: if not (space.width_class == .expanded or space.width_class == .wide) { 12rem } else if s.width <= 500 { 18rem } else if s.width < 900 { 30rem } else { 44rem }) {
        on activate: focus_strip(v, s.key)
        strip_controls(a, s, shown)
        if placed.len() > 1 {
            row(gap: .tight, align: .center, wrap: true) {
                for p in placed {
                    tab_chip(p, p.key == current)
                }
            }
        }
        if let p = shown {
            place_view(p)
        } else {
            ui.EmptyState("Nothing here", inline: true)
        }
    }
}

/// A strip's controls under its name: its width, one of the three, and the controls of the place in
/// front (its pin, closing it, moving it to another row of the desk), each committing what a drag
/// would. A width is one `resize-strip`, a place moved out one `move-place`, both attributed and
/// undoable like a drag.
view strip_controls(a: Arrangement, s: ArrangementStrip, shown: Option<ArrangementPlace>) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.MenuButton("Width", icon: .columns_2, icon_only: true, role: .plain) {
            ui.MenuItem("Narrow", checked: .some(width_of(s.width) == .narrow)) {
                on choose: session.resize_strip(strip: arranged(s.key), permille: permille_of(.narrow))
            }
            ui.MenuItem("Normal", checked: .some(width_of(s.width) == .normal)) {
                on choose: session.resize_strip(strip: arranged(s.key), permille: permille_of(.normal))
            }
            ui.MenuItem("Wide", checked: .some(width_of(s.width) == .wide)) {
                on choose: session.resize_strip(strip: arranged(s.key), permille: permille_of(.wide))
            }
        }
        if let p = shown {
            place_controls(a, p)
        }
    }
}

/// One tab of a column: the place it stands for, lit when it is the place in front.
view tab_chip(p: ArrangementPlace, current: Bool) {
    ui.Chip(place_title(p), selected: .some(current))
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
        conversation(r, fill)
    } else if let r = placed_resource(p.record) {
        if p.shown_as != "" or meta(r).opens {
            show(r, view: p.shown_as, fill)
        } else {
            ui.WindowWell(place: p.key, min: 12rem, fill)
        }
    } else {
        ui.WindowWell(place: p.key, min: 12rem, fill)
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
            on press: session.pin_place(place: arranged(p.key), pinned: .some(not p.pinned))
        }
        ui.Button("Close place", icon: .x, icon_only: true, role: .plain) {
            on press: session.close_place(place: arranged(p.key))
        }
        if a.rows.len() > 1 {
            ui.MenuButton("Move to row", icon: .navigation, icon_only: true, role: .plain) {
                for r in a.rows {
                    if r.key != strip_row(a, p.strip) {
                        ui.MenuItem(r.name) {
                            on choose: session.move_place(place: arranged(p.key), row: arranged(r.key))
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
