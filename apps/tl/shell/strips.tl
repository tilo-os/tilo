/// The strips shell: the person's layout drawn as rows of windows (docs/designs/app-language/
/// shell.md §2, §3). A *row* is a band across the shell holding *strips*, a *strip* is a column of
/// *places*, and a *place* is one window. A window is a window: its strip fills the room the row
/// gives it, its bar runs across its top edge, and its content fills everything under that one
/// bar. A strip of several places shows them as its tabs, the place in front filling the window;
/// a strip of one place is a window of one, its bar the place's own header — or, for a place
/// whose app draws none (a terminal, a foreign window), the frame's bar naming it, and on a
/// window nobody is in, that window's decorations alone.
///
/// The frame is `ui.WindowFrame`: one window bar for the whole shell, with the window's own
/// controls in a pill of the floating material at its trailing end, so a person sees the same
/// bar here as on the frames, the frameless canvas and the traditional desktop. Every arranging change is
/// one of the session's published actions (`shell:layout`): a press, a tab, a script and an
/// agent arrange the same way, attributed and undoable.
///
/// Rows of several strips stand side by side across the row's width, each strip taking the share
/// its own width names against the rest: a row of more windows than it holds gives each a
/// narrower window rather than pushing one off the screen, so every window of the row stays on it,
/// on a phone as on a laptop.
/// The places of the strip `strip`, in the order the document keeps them.
fn places_of(a: Layout, strip: Text) -> List<LayoutPlace> {
    a.places.filter(p -> p.strip == strip)
}

/// The strips the row `row` holds, in the document's order.
fn strips_of(a: Layout, row: Text) -> List<LayoutStrip> {
    a.strips.filter(s -> s.row == row)
}

/// The rows of the workspace `workspace`, in the document's order.
fn rows_of(a: Layout, workspace: Text) -> List<LayoutRow> {
    a.rows.filter(r -> r.workspace == workspace)
}

/// The strip the shell shows in front: the one whose window has the keyboard on this device, as
/// the session reads it, or the window that came last while none has.
fn strip_in_front(a: Layout) -> Text {
    if a.front != "" { a.front } else { a.strips.last()?.key ?? "" }
}

/// The strips maximized: each shows alone, filling the shell, the others behind
/// it. Empty while none is.
fn maximized_strips(a: Layout) -> List<LayoutStrip> {
    a.strips.filter(s -> s.maximized)
}

/// The style switch: every style every installed package offers, the one in use lit, so a
/// person moves between the shipped four and any they or their agent wrote, as shell.md §6
/// picks. Each is listed by its own doc line, in its author's words. Switching keeps every
/// place where it was, drawn the other way. The shell carries it in its own bar (`style_menu`);
/// this is the page Settings shows.
view style_switch(v: Viewing) {
    row(gap: .tight, align: .center, wrap: true) {
        for style in device.styles.offered {
            if style_listed(style) {
                ui.Chip(style_label(style), selected: v.style == style.style) {
                    on press: show_style(v, style.style)
                }
            }
        }
    }
}

/// A style's one name everywhere — the bar's menu title, its menu items and the Settings page:
/// its doc line, or its view's name when its author wrote none. A style a package offers but
/// cannot name — a package that carries no description that reads — is named by its package, so
/// no menu lists a blank row a person cannot read or pick.
fn style_label(style: device.Style) -> Text {
    if style.line != "" {
        style.line
    } else if style.name != "" { style.name } else { style.package }
}

/// Whether a style is one a person can pick: a named style, or one whose package is named. A
/// style nothing names (no view, no doc line, no package) stands for nothing in the shell, so
/// the switch lists none: an empty row is a band of the panel with nothing in it.
fn style_listed(style: device.Style) -> Bool {
    style.line != "" or style.name != "" or style.package != ""
}

/// The style the shell shows, named in the bar's menu by the same name it is listed under
/// (`style_label`): one name for a style in the bar and in Settings, never a different one in the
/// bar than in the one in the list.
fn style_title(v: Viewing) -> Text {
    if let chosen = device.styles.offered.find(s -> s.style == style_of(v)) {
        return style_label(chosen)
    }
    "Strips"
}

/// The style the shell shows as a menu of the bar's own: the same switch the Settings page
/// offers, where the person passes it in the shell, so the shell's windows start at the bar's edge
/// and no second row stands between them and it. The menu names the style the shell shows, so the
/// bar says what a person is looking at; its items list every style by its own doc line.
view style_menu(v: Viewing) {
    ui.MenuButton(
        "Showing {style_title(v)}",
        icon: .layout_grid,
        role: .bar,
        icon_only: space.compact,
    ) {
        for style in device.styles.offered {
            if style_listed(style) {
                ui.MenuItem(style_label(style), checked: v.style == style.style) {
                    on choose: show_style(v, style.style)
                }
            }
        }
        // The one settings place a style's own settings live in (`shell.md` §6): the menu opens
        // them where the style was picked, so a person changes the shell they look at without
        // hunting for an app. The entry says which
        // style's settings it opens, by the same name the menu lists the style under.
        ui.MenuItem("Settings for {style_title(v)}", icon: .settings) {
            on choose: open_style_settings(shell)
        }
    }
}

/// The shell's windows, filling the room between the bar and the dock: the maximized strips alone
/// when one is, else each row of the workspace in front with its strips side by side. The rows
/// stand in a shell that fills the room it is given, so a window takes the shell's height and its
/// app is the whole of it under the window's one bar.
///
/// A shell holds every workspace the person is in and shows one: the one in front (the session's
/// `current`). The others are a switch away in the bar, not a second shell under this one, so the
/// view draws the front workspace's rows and no other's — two workspaces side by side would halve
/// the room every window has, and an empty one would take half of it for nothing.
view strips_view(a: Layout, lists: Bool) {
    let maximizing = maximized_strips(a)
    if not maximizing.is_empty() {
        row(gap: .group, fill) {
            for s in maximizing {
                strip_window(a, s, lists, fill(1))
            }
        }
    } else if let workspace = shown_workspace(a) {
        scroll(axis: .both) {
            workspace_rows(a, workspace.key, lists, fill(1))
        }
    } else {
        // A shell the session has published no workspace of yet: it holds nothing to draw.
        scroll(axis: .both) {
            stack(gap: .group, fill) {
                ui.EmptyState("No window here yet", inline: true)
            }
        }
    }
}

/// The workspace the shell shows: the one in front, else the first it holds — a shell that has not
/// said which is in front still shows its first rather than nothing.
fn shown_workspace(a: Layout) -> Option<LayoutWorkspace> {
    if let current = a.workspaces.find(w -> w.current) {
        return current
    }
    a.workspaces.first()
}

/// The rows of one workspace, filling the room the shell gives them: a workspace's rows stand
/// under one another, each the height of its share of the shell.
view workspace_rows(a: Layout, workspace: Text, lists: Bool) {
    stack(gap: .group, fill) {
        for held in rows_of(a, workspace) {
            rows_of_the_shell(a, held, lists, fill(1))
        }
    }
}

/// One row of the shell: the band across it, with its strips side by side along it, each the share
/// of the shell its width names, so two windows the shell holds stand side by side and each app
/// keeps its room (`row_window`). A row holding more windows than the shell shows scrolls along
/// it, as the chrome's own row does. A phone's output is too narrow for two
/// windows side by side, so there it shows the strip in front alone, filling the shell, as the
/// chrome's shell does.
///
/// A row is not named here: the strip a person is in is what the bar's strip pill says, and the
/// row itself carries no label of its own (docs/design-language.md, `BarChip`).
view rows_of_the_shell(a: Layout, held: LayoutRow, lists: Bool) {
    let strips = strips_of(a, held.key)
    let front = strips.find(s -> s.key == strip_in_front(a)) ?? strips.last()
    stack(gap: .related, fill) {
        if space.compact {
            if let s = front {
                strip_window(a, s, lists, fill(1))
            } else {
                ui.EmptyState("No window here yet", inline: true)
            }
        } else {
            scroll(axis: .horizontal, fill) {
                row(gap: .group, align: .stretch, fill) {
                    for s in strips {
                        row_window(a, s, lists)
                    }
                    if strips.is_empty() {
                        ui.EmptyState("No window here yet", inline: true)
                    }
                }
            }
        }
    }
}

/// One strip's window as its row stands it: its share of the shell, the per-mille width the person
/// gave it (a usual window where it names none), so two windows the shell holds stand half each
/// and a lone window takes the room its neighbours left. Never narrower than 44rem — the room a
/// window's bar, its tabs and an app's own layout need — so a row of more windows than the shell
/// holds keeps their width and scrolls along it, as the chrome's own row does.
view row_window(a: Layout, s: LayoutStrip, lists: Bool) {
    strip_window(a, s, lists, fill: if s.width <= 0 { 667 } else { s.width }, min: 44rem)
}

/// One strip: the window its places make, filling the room it is given. Its bar runs across its
/// top edge — the tabs of its places while it shows several, the place's own header while it
/// shows one — and the place in front fills everything under it. A strip holding nothing is an
/// empty window, saying so.
///
/// The style's own setting `strips_tabs` (`chrome.tl`) says whether a strip of several places
/// shows its tab bar: off, the place in front fills the window alone and the others stay a tab
/// of it in the document, reached by the window's menu, for a person who wants the room.
///
/// While it shows them, a strip of several places has tabs a person rearranges: choosing one
/// shows its place and brings the strip forward, a press on its close takes that place out of
/// the strip, a drag within the bar ranks it among the others, and a drag out of the bar into
/// the shell gives it a window of its own (or, when the shell is the traditional one, the window
/// of the place it was let go under). Every one is an arranging action of the session's, so a
/// press, a key and an agent make the same change, attributed and undoable. A window's bar
/// likewise carries its place's document, so another window's bar takes it as a tab.
view strip_window(a: Layout, s: LayoutStrip, lists: Bool) {
    let in_front = s.key == strip_in_front(a)
    let placed = places_of(a, s.key)
    let chosen = placed.find(p -> p.key == s.active)
    let shown = chosen ?? placed.first()
    ui.DropTarget {
        on drop(target: host.Resource): tab_place_here(a, s, target)
        ui.WindowFrame(
            name: window_name(shown),
            focused: in_front,
            lists_actions: lists_the_shown(shown),
            carries: window_resource(shown),
            fill,
        ) {
            // A press on the window's bar brings it forward, the keyboard in it.
            on press: bring_forward(a, s)
            // The window fills the room the drop target around it is given: the target only
            // takes what is dropped on it, and the window keeps the strip's whole height.
            controls: {
                window_controls(a, s, shown, in_front, lists)
            }
            tabs: {
                if placed.len() > 1 and shell.strips_tabs {
                    ui.TabStrip(place_in_front(s), focused: in_front) {
                        on activate(p: session.Place): show_place(p)
                        on close(p: session.Place): session.close(place: p)
                        on reorder(p: session.Place, at: Int): reorder_place(a, s, p, at)
                        on detach(p: session.Place): detach_place(p)
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
                    ui.EmptyState("Nothing here", inline: true)
                }
            }
        }
    }
}

/// The resource a window's bar carries when the person picks it up: the document its place in front
/// shows, or empty when the place shows none (a window's well). A bar that carries nothing is not
/// lifted, so a foreign window's bar is no drag at all.
fn window_resource(shown: Option<LayoutPlace>) -> Text {
    if let p = shown { p.resource } else { "" }
}

/// Tab the place whose window's bar was dragged onto the strip `s` into that strip: a person drags
/// one window's bar onto another's to make its tabs, and the window's menu's "Move into … as a
/// tab" runs the same arranging action. The place lands after the strip's other tabs and
/// comes to the front of them, so the window shows what was dropped on it. A place already in
/// this strip is only brought to the front, so dropping a window on itself raises it rather than
/// opening a second place. The strip comes forward with it.
action tab_place_here(a: Layout, s: LayoutStrip, target: host.Resource) (
    icon: .columns_2,
    label: "Tab it here",
) {
    if let p = place_showing(target.id()) {
        if p.strip != s.key {
            session.move(
                place: the_place(p.key),
                row: the_row(s.row),
                strip: the_strip(s.key),
                before: .none,
            )
        }
        session.activate(place: the_place(p.key))
    } else { bring_forward(a, s) }
}

/// The document key of the place a tab names: its resource id is `shell:place:<key>`, so the key is
/// the part after the last colon. What a tab's event carries, read back for the actions that move
/// and split places by their key.
fn place_key(place: session.Place) -> Text {
    place.id().split(":").last() ?? ""
}

/// Rank the place `place` at `at` among its strip's tabs: what dragging a pill within the
/// window's bar does, as the shell's own `move` with the place it goes before. The strip reports
/// the pill's place among the tabs with the moved pill taken out, so the tab it lands before is
/// that index of the others; the last place goes last.
action reorder_place(a: Layout, s: LayoutStrip, place: session.Place, at: Int) (
    icon: .arrow_up_down,
    label: "Rearrange",
) {
    let key = place_key(place)
    let placed = places_of(a, s.key)
    let rest = placed.filter(p -> p.key != key)
    if let b = rest.at(at) {
        session.move(
            place: the_place(key),
            row: the_row(s.row),
            strip: the_strip(s.key),
            before: the_place(b.key),
        )
    } else {
        session.move(
            place: the_place(key),
            row: the_row(s.row),
            strip: the_strip(s.key),
            before: .none,
        )
    }
}

/// Give the place `place` a window of its own: what dragging a tab out of its window's bar does,
/// as the shell's own `split-right` for the window in front. The place stands in front of its new
/// window, and that window comes to the front of the shell.
action detach_place(place: session.Place) (icon: .columns_2, label: "Its own window") {
    session.split_right(tab: place_key(place))
}

/// What a strip as a whole is called: the place in front, or an empty strip.
fn window_name(shown: Option<LayoutPlace>) -> Text {
    if let p = shown { place_title(p) } else { "Empty strip" }
}

/// The place in front of `s`, as another strip's menu names it while it offers where a place
/// can join.
fn strip_place(a: Layout, s: LayoutStrip) -> Option<LayoutPlace> {
    let placed = places_of(a, s.key)
    let chosen = placed.find(p -> p.key == s.active)
    chosen ?? placed.first()
}

/// The resource of the place in front of `s`, as its tab names it: empty while the strip has no
/// place in front, which lights its first tab instead.
fn place_in_front(s: LayoutStrip) -> Option<session.Place> {
    if s.active != "" { the_place(s.active) }
}

/// Shows the place `place` and brings its window forward with it, the keyboard in it: what
/// choosing a tab does, and the same arranging action a script or an agent runs.
action show_place(place: session.Place) {
    session.activate(place: place)
}

/// Brings the window of the strip `s` to the front, the keyboard in it: the session brings its
/// place in front forward, as it does for a press, a script or an agent alike. A strip with no
/// place has no window to bring.
action bring_forward(a: Layout, s: LayoutStrip) (icon: .app_window, label: "Bring to front") {
    if let p = strip_place(a, s) {
        session.activate(place: the_place(p.key))
    }
}

/// A window's own controls, in the pill at the trailing end of its one bar: the window's menu,
/// which lists the actions its resource publishes and then its width, its coming to the front,
/// and the place's pin and row, then maximize or restore and close. A bar shows one "…": each
/// control is one action of the session's or the app's, so a press and an agent change the shell
/// the same way, and none of them ever asks for a bar of its own. Where the shell's bar already
/// names the window in front's resource (`listed`), this menu keeps the shell's controls alone,
/// so every action stands once.
view window_controls(
    a: Layout,
    s: LayoutStrip,
    shown: Option<LayoutPlace>,
    in_front: Bool,
    listed: Bool,
) {
    row(gap: .tight, align: .center) {
        ui.MenuButton(
            "This window",
            icon: .more_horizontal,
            icon_only: true,
            role: .plain,
            target: if let p = shown { menu_resource(p, listed) },
        ) {
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
                    on choose: bring_forward(a, s)
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

/// Whether the window's own bar names the resource its place shows, so the host fills its one
/// menu with that resource's actions. The frame is told this (`ui.WindowFrame(lists_actions:)`),
/// so the app's own header keeps its title and the shell's controls alone and every action
/// stands once, in the one list the person reaches.
fn lists_the_place(p: LayoutPlace) -> Bool {
    if let r = placed_resource(p.resource) { true } else { false }
}

/// [`lists_the_place`] for a window that may show nothing: a strip holding no place lists none.
fn lists_the_shown(p: Option<LayoutPlace>) -> Bool {
    if let shown = p { lists_the_place(shown) } else { false }
}

/// The resource whose published actions the window's one menu lists, for the host to fill from
/// the declarations: the document the place shows, and no resource where another bar already names
/// it for the window in front (`listed`, the traditional desktop's `action_bar`). A place that
/// shows no document this store holds (a foreign window) offers none, and its menu keeps the
/// shell's own commands alone.
fn menu_resource(p: LayoutPlace, listed: Bool) -> Option<host.Resource> {
    if not listed { placed_resource(p.resource) }
}

/// A place's own controls: its window's menu, its pinned mark, its pin, and its close. Outside
/// the well, so a press there is the shell's and one on the well is the app's. The canvas's
/// framed places carry these alone: they are not a strip's window, so its width and its way to
/// the front are not theirs.
view place_controls(a: Layout, p: LayoutPlace, listed: Bool) {
    row(gap: .tight, align: .center) {
        ui.MenuButton(
            "This window",
            icon: .more_horizontal,
            icon_only: true,
            role: .plain,
            target: menu_resource(p, listed),
        ) {
            if a.rows.len() > 1 {
                for r in a.rows {
                    if r.key != strip_row(a, p.strip) {
                        ui.MenuItem(r.name) {
                            on choose: session.move(place: the_place(p.key), row: the_row(r.key))
                        }
                    }
                }
            }
        }
        if p.pinned {
            ui.Button("Pin place", icon: .pin, icon_only: true, toggled: true, role: .plain) {
                on press: session.pin(place: the_place(p.key), pinned: false)
            }
        } else {
            ui.Button("Pin place", icon: .pin, icon_only: true, role: .plain) {
                on press: session.pin(place: the_place(p.key), pinned: true)
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
/// Every style draws a place through this one view — a strip's body, a framed canvas pin, a
/// frameless place on the canvas — so a place looks the same whichever style shows it.
view place_content(p: LayoutPlace) {
    place_view(p, fill)
}

/// A place's content: its resource drawn inline through the view it names, as its own app's code
/// (shell.md §8, "The canvas composes views"), or the well the app's window shows in when there
/// is no resource here to draw (a terminal, a legacy window). The window's controls are the
/// frame's, in its one bar, outside it.
///
/// The window the place shows says where it stands: a window that runs on another device shows
/// its notice instead of an empty well, and one whose app stopped answering keeps its content
/// under the hung card, so the person answers the app where they are working.
view place_view(p: LayoutPlace) {
    if let w = window_showing(p) {
        if w.elsewhere {
            window_elsewhere(w)
        } else if w.not_responding {
            layers(fill) {
                place_body(p, fill)
                hung_card(w)
                asked_over(w)
            }
        } else if not asked_in(w).is_empty() {
            layers(fill) {
                place_body(p, fill)
                asked_over(w)
            }
        } else {
            place_body(p)
        }
    } else {
        place_body(p)
    }
}

/// What a place draws when its window is this device's: its resource through its view, or the well
/// the window shows in.
view place_body(p: LayoutPlace) {
    if let q = query_of(p.key) {
        query_view(q, p)
    } else if let r = placed_resource(p.resource) {
        if p.shown_as != "" or meta(r).opens {
            show(r, view: p.shown_as, fill)
        } else {
            ui.WindowWell(place: p.key, fill)
        }
    } else {
        ui.WindowWell(place: p.key, fill)
    }
}

/// The window the place `p` shows, as the session lists it: a place's key is its window's tab, so
/// the device's windows say whether it is here and whether its app still answers.
fn window_showing(p: LayoutPlace) -> Option<device.Window> {
    return device.windows.all.find(w -> w.key == p.key)
}

/// The query the place `key` shows, while its app answers it; `.none` for a place showing a
/// resource (shell.md §8, "Finding what to place").
fn query_of(key: Text) -> Option<LayoutQuery> {
    let a = root Layout
    a.queries.find(q -> q.key == key)
}

/// A place's body while it shows a query (shell.md §8): what the person searched for and where,
/// the rows it found (each a press that places that match in this same place), and what became
/// of it — still searching, nothing matched (with Retry), or the app's reason (with Retry).
view query_view(q: LayoutQuery, p: LayoutPlace) {
    stack(gap: .related, fill) {
        ui.Text(query_heading(q), role: .label, secondary: true)
        if q.state == "searching" {
            ui.ProgressBar()
        } else if q.state == "failed" {
            ui.Text(q.reason)
            ui.Button("Retry", icon: .refresh) {
                on press: query_retry(p.key)
            }
        } else if q.state == "none" {
            ui.Text("Nothing matched {q.text}")
            ui.Button("Retry", icon: .refresh) {
                on press: query_retry(p.key)
            }
        } else {
            for m in refs_of(q.key) {
                if let r = resource(m.address) {
                    ui.Button(m.title) {
                        on press: query_show(p.key, r)
                    }
                }
            }
        }
    }
}

/// The resources the query `key` found, in the order the app answered them.
fn refs_of(key: Text) -> List<LayoutRef> {
    let a = root Layout
    a.refs.filter(r -> r.query == key)
}

/// What a query's place says while it runs: "Looking for <text> in <app>…".
fn query_heading(q: LayoutQuery) -> Text {
    "Looking for {q.text} in {q.in_app}"
}

/// The row the strip `strip` stands in, by the place it holds.
fn strip_row(a: Layout, strip: Text) -> Text {
    a.strips.find(s -> s.key == strip)?.row ?? ""
}

/// The workspace the strip `strip` stands in: a row is one workspace's, so what a place moves
/// within is that workspace's strips and rows.
fn strip_workspace(a: Layout, strip: Text) -> Text {
    a.strips.find(s -> s.key == strip)?.workspace ?? ""
}
