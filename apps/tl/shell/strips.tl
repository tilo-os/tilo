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
/// its own width names against the rest: a row of more windows than it holds gives each a
/// narrower window rather than pushing one off the desk, so every window of the row stays on it,
/// on a phone as on a laptop.
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

/// The strip the shell shows in front: the one the person brought forward, or the window that
/// came to the desk last, so the desk's front follows what arrived on it.
fn strip_in_front(a: Arrangement, v: Viewing) -> Text {
    if v.focused != "" { v.focused } else { a.strips.last()?.key ?? "" }
}

/// The strips standing maximized: each shows alone as the window the desk is, the others behind
/// it. Empty while none is.
fn maximized_strips(a: Arrangement) -> List<ArrangementStrip> {
    a.strips.filter(s -> s.maximized)
}

/// The style switch: every desk style every installed package offers, the one in use lit, so a
/// person moves between the shipped four and any they or their agent wrote, as shell.md §6
/// picks. Each is listed by its own doc line, in its author's words. Switching keeps every
/// place where it was, drawn the other way. The desk carries it in its own bar (`style_menu`);
/// this is the page Settings shows.
view style_switch(v: Viewing, d: Desk) {
    row(gap: .tight, align: .center, wrap: true) {
        for style in d.styles {
            ui.Chip(style_label(style), selected: v.style == style.style) {
                on press: show_style(v, style.style)
            }
        }
    }
}

/// A style's one name everywhere — the bar's menu title, its menu items and the Settings page:
/// its doc line, or its view's name when its author wrote none, so no style is listed by an
/// empty chip.
fn style_label(style: DeskStyle) -> Text {
    if style.line != "" { style.line } else { style.name }
}

/// The style the desk shows, named in the bar's menu by the same name it is listed under
/// (`style_label`): one name for a style in the bar and in Settings, never a different one in the
/// seat from the one in the list.
fn style_title(v: Viewing, d: Desk) -> Text {
    if let chosen = d.styles.find(s -> s.style == style_of(v)) {
        return style_label(chosen)
    }
    "Strips"
}

/// The style the desk shows as a menu of the bar's own: the same switch the Settings page
/// offers, where the person passes it on the desk, so the desk's windows start at the bar's edge
/// and no second row stands between them and it. The menu names the style the desk shows, so the
/// bar says what a person is looking at; its items list every style by its own doc line.
view style_menu(v: Viewing, d: Desk) {
    ui.MenuButton(
        "Showing {style_title(v, d)}",
        icon: .layout_grid,
        role: .bar,
        icon_only: space.compact,
    ) {
        for style in d.styles {
            ui.MenuItem(style_label(style), checked: v.style == style.style) {
                on choose: show_style(v, style.style)
            }
        }
    }
}

/// The desk's windows, filling the room between the bar and the dock: the maximized strips alone
/// when one is, else each row of the workspace in front with its strips side by side. The rows
/// stand in a desk that fills the room it is given, so a window is as tall as the desk and its
/// app is the whole of it under the window's one bar.
///
/// A desk holds every workspace the person is in and shows one: the one in front (the session's
/// `current`). The others are a switch away in the bar, not a second desk under this one, so the
/// view draws the front workspace's rows and no other's — two workspaces side by side would halve
/// the room every window has, and an empty one would take a desk's half for nothing.
view strips_view(a: Arrangement, v: Viewing, lists: Bool) {
    let maximizing = maximized_strips(a)
    if not maximizing.is_empty() {
        row(gap: .group, fill) {
            for s in maximizing {
                strip_window(a, v, s, lists, fill(1))
            }
        }
    } else if let workspace = shown_workspace(a) {
        scroll(axis: .both) {
            workspace_rows(a, v, workspace.key, lists, fill(1))
        }
    } else {
        // A desk the session has published no workspace of yet: it holds nothing to draw.
        scroll(axis: .both) {
            stack(gap: .group, fill) {
                ui.EmptyState(
                    "No window here yet",
                    message: "Open something into this strip.",
                    inline: true,
                )
            }
        }
    }
}

/// The workspace the desk shows: the one in front, else the first it holds — a desk that has not
/// said which is in front still shows its first rather than nothing.
fn shown_workspace(a: Arrangement) -> Option<ArrangementWorkspace> {
    if let current = a.workspaces.find(w -> w.current) {
        return current
    }
    a.workspaces.first()
}

/// The rows of one workspace, filling the room the desk gives them: a workspace's rows stand
/// under one another, each the height of its share of the desk.
view workspace_rows(a: Arrangement, v: Viewing, workspace: Text, lists: Bool) {
    stack(gap: .group, fill) {
        for held in rows_of(a, workspace) {
            rows_of_the_desk(a, v, held, lists, fill(1))
        }
    }
}

/// One row of the desk: the band across it, with its strips side by side along it, each the share
/// of the desk its width names, so two windows the desk holds stand side by side and its app keeps
/// its room (`row_window`). A row holding more windows than the desk shows scrolls along it, as the
/// chrome's own row does. A phone's output is too narrow for two
/// windows side by side, so there it shows the strip in front alone, filling the desk, as the
/// chrome's desk does.
///
/// A row is not named here: the strip a person is in is what the bar's strip pill says, and the
/// desk itself carries no label of its own (docs/design-language.md, `BarChip`).
view rows_of_the_desk(a: Arrangement, v: Viewing, held: ArrangementRow, lists: Bool) {
    let strips = strips_of(a, held.key)
    let front = strips.find(s -> s.key == strip_in_front(a, v)) ?? strips.last()
    stack(gap: .related, fill) {
        if space.compact {
            if let s = front {
                strip_window(a, v, s, lists, fill(1))
            } else {
                ui.EmptyState(
                    "No window here yet",
                    message: "Open something into this strip.",
                    inline: true,
                )
            }
        } else {
            scroll(axis: .horizontal, fill) {
                row(gap: .group, align: .stretch, fill) {
                    for s in strips {
                        row_window(a, v, s, lists)
                    }
                    if strips.is_empty() {
                        ui.EmptyState(
                            "No window here yet",
                            message: "Open something into this strip.",
                            inline: true,
                        )
                    }
                }
            }
        }
    }
}

/// One strip's window as its row stands it: its share of the desk, the per-mille width the person
/// gave it (a usual window where it names none), so two windows the desk holds stand half each
/// and a lone window takes the room its neighbours left. Never narrower than 44rem — the room a
/// window's bar, its tabs and an app's own layout need — so a row of more windows than the desk
/// holds keeps their width and scrolls along it, as the chrome's own row does.
view row_window(a: Arrangement, v: Viewing, s: ArrangementStrip, lists: Bool) {
    strip_window(a, v, s, lists, fill: if s.width <= 0 { 667 } else { s.width }, min: 44rem)
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
    ui.WindowFrame(name: window_name(shown), focused: in_front, lists_actions: lists) {
        controls: {
            window_controls(a, v, s, shown, in_front)
        }
        tabs: {
            if placed.len() > 1 {
                ui.TabStrip(place_in_front(s), focused: in_front) {
                    on activate(p: session.Place): show_place(v, s.key, p)
                    on close(p: session.Place): session.close(place: p)
                    for p in placed {
                        ui.TabItem(the_place(p.key), title: place_title(p))
                    }
                }
            }
        }
        body: {
            if let p = shown {
                place_content(p)
            } else {
                ui.EmptyState(
                    "Nothing here",
                    message: "Open something into this strip.",
                    inline: true,
                )
            }
        }
    }
}

/// What a strip as a whole is called: the place in front, or an empty strip.
fn window_name(shown: Option<ArrangementPlace>) -> Text {
    if let p = shown { place_title(p) } else { "Empty strip" }
}

/// The place in front of `s`, as another strip's menu names it while it offers where a place
/// can join.
fn strip_place(a: Arrangement, s: ArrangementStrip) -> Option<ArrangementPlace> {
    let placed = places_of(a, s.key)
    let chosen = placed.find(p -> p.key == s.active)
    chosen ?? placed.first()
}

/// The resource of the place in front of `s`, as its tab names it: empty while the strip has no
/// place in front, which lights its first tab instead.
fn place_in_front(s: ArrangementStrip) -> Option<session.Place> {
    if s.active != "" { the_place(s.active) }
}

/// Shows the place `place` and brings its strip forward with it: what choosing a tab does, and
/// the same arranging action a script or an agent runs. The strip's own reading moves first, the
/// desk then brings the place in front of its strip.
action show_place(v: Viewing, strip: Text, place: session.Place) {
    focus_strip(v, strip)
    session.activate(place: place)
}

/// A window's own controls, in the pill at the trailing end of its one bar: the window's menu
/// (its width, coming to the front, and the place's pin and row), maximize or restore, and
/// close. Three controls whatever the place holds, so the pill still fits a window on a phone;
/// nothing about the place but its close stands in the bar of its own. Each is one arranging
/// action of the session's, so a press and an agent change the desk the same way, and none of
/// them ever asks for a bar of its own.
view window_controls(
    a: Arrangement,
    v: Viewing,
    s: ArrangementStrip,
    shown: Option<ArrangementPlace>,
    in_front: Bool,
) {
    row(gap: .tight, align: .center) {
        ui.MenuButton("This window", icon: .more_horizontal, icon_only: true, role: .plain) {
            ui.MenuItem("Narrow", checked: width_of(s.width) == .narrow) {
                on choose: session.resize(strip: the_strip(s.key), permille: permille_of(.narrow))
            }
            ui.MenuItem("Normal", checked: width_of(s.width) == .normal) {
                on choose: session.resize(strip: the_strip(s.key), permille: permille_of(.normal))
            }
            ui.MenuItem("Wide", checked: width_of(s.width) == .wide) {
                on choose: session.resize(strip: the_strip(s.key), permille: permille_of(.wide))
            }
            if not in_front {
                ui.MenuItem("Bring to front", icon: .app_window) {
                    on choose: focus_strip(v, s.key)
                }
            }
            if let p = shown {
                if p.pinned {
                    ui.MenuItem("Unpin window", icon: .pin, checked: true) {
                        on choose: session.pin(place: the_place(p.key), pinned: false)
                    }
                } else {
                    ui.MenuItem("Pin window", icon: .pin) {
                        on choose: session.pin(place: the_place(p.key), pinned: true)
                    }
                }
                // The strip's places are its tabs: a strip of several shows them as its
                // tab strip. Moving a place into another strip's tab strip, or its own
                // beside another place, is the menu's way to make tabs without a drag
                // (shell.md §2). A strip of one place offers its own place as where to join.
                // Only this workspace's strips are offered: a row is one workspace's, and a
                // place moves within its workspace (shell.md §2, `move`).
                let here = strip_workspace(a, s.key)
                if a.strips.len() > 1 {
                    for other in a.strips {
                        if other.key != s.key and other.workspace == here {
                            ui.MenuItem(
                                "Move into {window_name(strip_place(a, other))} as a tab",
                                icon: .columns_2,
                            ) {
                                on choose: session.move(
                                    place: the_place(p.key),
                                    row: the_row(other.row),
                                    strip: the_strip(other.key),
                                    before: .none,
                                )
                            }
                        }
                    }
                }
                if a.rows.len() > 1 {
                    for r in a.rows {
                        if r.workspace == here and r.key != strip_row(a, p.strip) {
                            ui.MenuItem("Move to {r.name}", icon: .navigation) {
                                on choose: session.move(
                                    place: the_place(p.key),
                                    row: the_row(r.key),
                                    strip: .none,
                                    before: .none,
                                )
                            }
                        }
                    }
                }
            }
        }
        if s.maximized {
            ui.Button("Restore", icon: .minimize_2, icon_only: true, role: .plain) {
                on press: session.maximize_strip(strip: the_strip(s.key), on: false)
            }
        } else {
            ui.Button("Maximize", icon: .maximize_2, icon_only: true, role: .plain) {
                on press: session.maximize_strip(strip: the_strip(s.key), on: true)
            }
        }
        if let p = shown {
            ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
                on press: session.close(place: the_place(p.key))
            }
        }
    }
}

/// A place's own controls: its pinned mark, its pin, its move to another row, and its close.
/// Outside the well, so a press there is the shell's and one on the well is the app's. The
/// canvas's framed places carry these alone: they are not a strip's window, so its width and
/// its way to the front are not theirs.
view place_controls(a: Arrangement, p: ArrangementPlace) {
    row(gap: .tight, align: .center) {
        if p.pinned {
            ui.Button("Pin place", icon: .pin, icon_only: true, toggled: true, role: .plain) {
                on press: session.pin(place: the_place(p.key), pinned: false)
            }
        } else {
            ui.Button("Pin place", icon: .pin, icon_only: true, role: .plain) {
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
/// is no resource here to draw (a terminal, a legacy window). The window's controls are the
/// frame's, in its one bar, outside it.
view place_view(p: ArrangementPlace) {
    if let r = placed_resource(p.record) {
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
    a.strips.find(s -> s.key == strip)?.row ?? ""
}

/// The workspace the strip `strip` stands in: a row is one workspace's, so what a place moves
/// within is that workspace's strips and rows.
fn strip_workspace(a: Arrangement, strip: Text) -> Text {
    a.strips.find(s -> s.key == strip)?.workspace ?? ""
}
