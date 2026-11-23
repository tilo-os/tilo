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
        if workspace_holds_nothing(a, workspace.key) {
            start_cards()
        } else {
            scroll(axis: .both) {
                workspace_rows(a, workspace.key, lists, fill(1))
            }
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

/// Whether the workspace `workspace` shows no window at all: every strip it holds is empty, so
/// the shell has nothing to draw and offers the start cards instead.
fn workspace_holds_nothing(a: Layout, workspace: Text) -> Bool {
    for s in a.strips.filter(s -> s.workspace == workspace) {
        if not places_of(a, s.key).is_empty() {
            return false
        }
    }
    return true
}

/// The start cards on a shell with no window, one set on each display that shows none: what the
/// person can open, as one card each, and the shell's own places beside them. A press opens an
/// app in the workspace in front through the same action the launcher, a script and an agent use;
/// with nothing to show, the shell is a way in rather than a blank band.
view start_cards() {
    page(full) {
        stack(gap: .section) {
            stack(gap: .tight) {
                ui.Text("Start something", role: .title)
                ui.Text(
                    "Only you see what you make here until you invite someone.",
                    role: .caption,
                    secondary: true,
                )
            }
            grid(min: 18rem, gap: .related) {
                ui.Card(title: "Notes") {
                    row(gap: .related, align: .center) {
                        ui.Icon(.file_text, size: .large)
                        stack(gap: .tight) {
                            ui.Text("Notes", role: .label)
                            ui.Text("Write ideas down", role: .caption, secondary: true)
                        }
                    }
                    on activate: session.launchpad(card: "notes")
                }
                ui.Card(title: "Tasks") {
                    row(gap: .related, align: .center) {
                        ui.Icon(.list_checks, size: .large)
                        stack(gap: .tight) {
                            ui.Text("Tasks", role: .label)
                            ui.Text("Write down what needs doing", role: .caption, secondary: true)
                        }
                    }
                    on activate: session.launchpad(card: "tasks")
                }
                ui.Card(title: "Invite people") {
                    row(gap: .related, align: .center) {
                        ui.Icon(.user_plus, size: .large)
                        stack(gap: .tight) {
                            ui.Text("Invite people", role: .label)
                            ui.Text("Work on this with someone", role: .caption, secondary: true)
                        }
                    }
                    on activate: session.launchpad(card: "invite")
                }
                ui.Card(title: "Catch up") {
                    row(gap: .related, align: .center) {
                        ui.Icon(.history, size: .large)
                        stack(gap: .tight) {
                            ui.Text("Catch up", role: .label)
                            ui.Text("See what changed and why", role: .caption, secondary: true)
                        }
                    }
                    on activate: session.launchpad(card: "activity")
                }
            }
            row(gap: .section, align: .center, wrap: true) {
                ui.KeyHints {
                    ui.KeyHint(["Arrows"], "Move")
                    ui.KeyHint(["Enter"], "Open")
                    ui.KeyHint(["Super", "N"], "Every app")
                }
                ui.Button("Open an app", icon: .layout_grid) {
                    on press: device.show_surface(device.overlays, surface: "launcher")
                }
            }
        }
    }
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
                window_controls(s, shown, lists)
            }
            tabs: {
                if placed.len() > 1 and shell.strips_tabs {
                    ui.TabStrip(place_in_front(s), focused: in_front) {
                        on activate(p: session.Place): show_place(p)
                        on close(p: session.Place): session.close(place: p)
                        on reorder(p: session.Place, at: Int): reorder_place(a, s, p, at)
                        on detach(p: session.Place): detach_place(p)
                        for p in placed {
                            if let w = shown_window(p) {
                                ui.TabItem(the_place(p.key), title: place_title(p), icon: w.icon)
                            } else {
                                ui.TabItem(the_place(p.key), title: place_title(p))
                            }
                        }
                    }
                }
            }
            body: {
                if let p = shown {
                    // A window whose app stopped answering says so on the window itself, with the
                    // two ways out: keep waiting, or quit it. The card stands above the content,
                    // and a window that answers keeps its body the whole room.
                    let w = shown_window(p)
                    if let window = w {
                        if window.not_responding {
                            stack(gap: .tight, fill) {
                                hung_window(window)
                                place_content(p)
                            }
                        } else {
                            place_content(p)
                        }
                    } else {
                        place_content(p)
                    }
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

/// A window's own controls, in the pill at the trailing end of its one bar: the window's menu and
/// then maximize or restore and close. A bar shows one "…": every row of the menu is one action of
/// the session's, so a press and an agent change the shell the same way, and none of them ever
/// asks for a bar of its own.
view window_controls(s: LayoutStrip, shown: Option<LayoutPlace>, listed: Bool) {
    row(gap: .tight, align: .center) {
        if let p = shown {
            if let w = shown_window(p) {
                window_menu(w, shown, listed)
                window_bar_actions(w)
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

/// The window the place `p` shows, as the session publishes its windows: a place and its window
/// share the tab's key, so a place's own controls offer the rows of the window it shows.
fn shown_window(p: LayoutPlace) -> Option<device.Window> {
    return device.windows.all.find(w -> w.key == p.key)
}

/// What a window's bar carries beside its tab, as the desktop's window bar does: another tab of
/// the same app, the window's own search, and the way back and forward through what it showed.
/// Each is one session action, so a press, a key and an agent move the window the same way.
view window_bar_actions(w: device.Window) {
    if w.opens_tabs and w.local {
        ui.Button("New tab of this window", icon: .plus, icon_only: true, role: .plain) {
            on press: session.open_app(app: w.app_key, beside: w.key)
        }
    }
    ui.Button("Search this window", icon: .search, icon_only: true, role: .plain) {
        on press: session.search()
    }
    ui.Button("History of this window", icon: .history, icon_only: true, role: .plain) {
        on press: session.activity(workspace: w.workspace_key, view: "catch-up", author: .none)
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

/// The window menu: everything the person can do to one window, in the one menu its bar carries
/// and the overview's cards carry too. Every row is a session action (`so.tilo.session`), so a
/// press, a script and an agent do the same thing and each change is attributed and undoable.
///
/// The rows follow the window: a window covering its display offers it back, one filling its
/// strip offers the strip back instead, the widest window cannot be widened and says so, a window
/// someone else holds can be neither named nor closed, and a window of one tab offers no split.
/// A place the window cannot go is not offered at all: no row for a choice that cannot be made.
///
/// The rows stand in runs of what they do to the window, a rule between one run and the next:
/// what it covers and fills, the share of its strip it takes, where it goes, what it shows
/// (another tab, its name, a split, its pin), stopping its app's control of the desktop, and
/// closing it.
view window_menu(w: device.Window, place: Option<LayoutPlace>, listed: Bool) {
    state renaming: Bool = false
    state typed: Text = w.title
    ui.MenuButton(
        "This window",
        icon: .more_horizontal,
        icon_only: true,
        role: .plain,
        target: if let p = place { menu_resource(p, listed) },
    ) {
        ui.MenuItem(
            "Reveal",
            icon: .corner_up_right,
            disabled: w.focused,
            reason: "It is in front already",
        ) {
            on choose: session.focus(window(w))
        }
        if w.fullscreen {
            ui.MenuItem("Exit full screen", icon: .minimize_2) {
                on choose: session.fullscreen(group: w.group_key, on: false)
            }
        } else {
            ui.MenuItem("Full screen", icon: .monitor) {
                on choose: session.fullscreen(group: w.group_key, on: true)
            }
        }
        if strip_maximized(w) {
            ui.MenuItem("Restore", icon: .minimize_2) {
                on choose: session.maximize_strip(strip: the_strip(layout_strip(w)), on: false)
            }
        } else {
            ui.MenuItem("Fill the strip", icon: .maximize_2) {
                on choose: session.maximize_strip(strip: the_strip(layout_strip(w)), on: true)
            }
        }
        if w.floating {
            ui.MenuItem("Snap back", icon: .layout_grid) {
                on choose: session.float(group: w.group_key, on: false)
            }
        } else {
            ui.MenuItem("Float this window", icon: .layers) {
                on choose: session.float(group: w.group_key, on: true)
            }
        }
        // The share of its strip the window takes, and the steps between the shares. A window at
        // the widest or the narrowest says so rather than stepping past.
        ui.MenuItem("Half the strip", checked: strip_width(w) == 500) {
            on choose: session.resize(strip: the_strip(layout_strip(w)), permille: 500)
        }
        ui.MenuItem("Narrow", checked: strip_width(w) == 333) {
            on choose: session.resize(strip: the_strip(layout_strip(w)), permille: 333)
        }
        ui.MenuItem("Normal", checked: strip_width(w) == 667) {
            on choose: session.resize(strip: the_strip(layout_strip(w)), permille: 667)
        }
        ui.MenuItem("Wide", checked: strip_width(w) == 1000) {
            on choose: session.resize(strip: the_strip(layout_strip(w)), permille: 1000)
        }
        ui.MenuItem("Every window the same") {
            on choose: equal_widths(w)
        }
        ui.MenuItem("Wider", disabled: strip_width(w) >= 1000, reason: "As wide as it gets") {
            on choose: session.resize(
                strip: the_strip(layout_strip(w)),
                permille: wider_than(strip_width(w)),
            )
        }
        ui.MenuItem("Narrower", disabled: strip_width(w) <= 333, reason: "As narrow as it gets") {
            on choose: session.resize(
                strip: the_strip(layout_strip(w)),
                permille: narrower_than(strip_width(w)),
            )
        }
        // One place along its strip, where there is a place to go.
        if let b = group_before(w) {
            ui.MenuItem("Move left", icon: .arrow_left) {
                on choose: move_before(w, b.group_key)
            }
        }
        if group_after(w) != .none {
            ui.MenuItem("Move right", icon: .arrow_right) {
                on choose: move_right(w)
            }
        }
        // Into another window of its workspace, as a tab of it: the window is a group of the
        // band they share.
        for s in layout.strips.filter(s -> s.workspace == w.workspace_key and s.key != w.group_key) {
            ui.MenuItem("Move into {menu_strip_name(s.key)} as a tab") {
                on choose: session.move_tab(
                    tab: w.key,
                    group: s.key,
                    strip: .none,
                    workspace: .none,
                    display: .none,
                )
            }
        }
        for o in layout.workspaces.filter(o -> o.key != w.workspace_key) {
            ui.MenuItem("Move to {o.name}") {
                on choose: session.move_tab(
                    tab: w.key,
                    workspace: o.key,
                    strip: .none,
                    display: .none,
                )
            }
        }
        for d in device.windows.displays {
            if d.id != w.display_id {
                ui.MenuItem("Display {d.number}") {
                    on choose: session.move_tab(
                        tab: w.key,
                        display: "{d.id}",
                        strip: .none,
                        workspace: .none,
                    )
                }
            }
        }
        for r in layout.rows.filter(r -> r.workspace == w.workspace_key and r.key != row_of(w)) {
            ui.MenuItem("Move to {r.name}") {
                on choose: session.move(place: the_place(w.key), row: the_row(r.key))
            }
        }
        // What the window shows and what it is called.
        if w.opens_tabs and w.local {
            ui.MenuItem("New tab of this app", icon: .plus) {
                on choose: session.open_app(app: w.app_key, beside: w.key)
            }
        }
        if w.renameable and w.local {
            ui.MenuItem("Rename", icon: .pencil) {
                on choose: set(renaming, true)
            }
        }
        if tabs_in_group(w) > 1 {
            ui.MenuItem("Split right", icon: .columns_2) {
                on choose: session.split_right(tab: w.key)
            }
            ui.MenuItem("New group", icon: .app_window) {
                on choose: session.new_group(tab: w.key)
            }
        }
        if pinned(w) {
            ui.MenuItem("Unpin window", icon: .pin, checked: true) {
                on choose: session.pin(place: the_place(w.key), pinned: false)
            }
        } else {
            ui.MenuItem("Pin window", icon: .pin) {
                on choose: session.pin(place: the_place(w.key), pinned: true)
            }
        }
        if w.controls_desktop {
            ui.MenuItem("Stop controlling the desktop", icon: .ban) {
                on choose: session.stop_desktop_control(group: w.group_key)
            }
        }
        if tabs_in_group(w) > 1 {
            ui.MenuItem("Close tab", icon: .x, destructive: true) {
                on choose: session.close_tab(window(w))
            }
        }
        ui.MenuItem("Close this window", icon: .x, destructive: true) {
            on choose: session.close_window(group: w.group_key)
        }
    }
    if renaming {
        ui.Popover(label: "Rename this window", open: renaming) {
            ui.TextInput(bind: typed, autofocus: true, placeholder: "A new name") {
                on submit: session.rename(tab: w.key, name: typed).then(set(renaming, false))
            }
            on dismiss: set(renaming, false)
        }
    }
}

/// The windows of the strip `strip` that stand in front of their group, leading to trailing: one
/// per window of the strip, whatever tabs stand behind it.
fn strip_groups(strip: Text) -> List<device.Window> {
    return device.windows.all
        .filter(v -> v.strip_key == strip and v.tab_index == 0)
        .sort_by(v -> v.place)
}

/// The window its group shows in front: the tab the menu is on, or its group's front tab while a
/// tab behind it carries the menu.
fn group_front(w: device.Window) -> Option<device.Window> {
    return device.windows.all
        .filter(v -> v.group_key == w.group_key)
        .sort_by(v -> v.tab_index)
        .first()
}

/// The window leading `w`'s along its strip, while one stands before it.
fn group_before(w: device.Window) -> Option<device.Window> {
    let front = group_front(w)
    if let f = front {
        return strip_groups(w.strip_key).filter(v -> v.place < f.place).last()
    }
    return .none
}

/// The window trailing `w`'s along its strip, while one stands after it.
fn group_after(w: device.Window) -> Option<device.Window> {
    let front = group_front(w)
    if let f = front {
        return strip_groups(w.strip_key).filter(v -> v.place > f.place).first()
    }
    return .none
}

/// Move the window `w` before the window `before` of its strip: one place along it.
action move_before(
    w: device.Window (label: "The window"),
    before: Text (label: "The window it goes before"),
) (icon: .arrow_left, label: "Move the window left") {
    session.move_tab(
        tab: w.key,
        strip: w.strip_key,
        before: before,
        workspace: .none,
        display: .none,
    )
}

/// Move the window `w` one place to the trailing side: before the window after the next, or
/// last in its strip while none stands there.
action move_right(w: device.Window (label: "The window")) (
    icon: .arrow_right,
    label: "Move the window right",
) {
    let next = group_after(w)
    if let n = next {
        let beyond = strip_groups(w.strip_key).filter(v -> v.place > n.place).first()
        if let b = beyond {
            session.move_tab(
                tab: w.key,
                strip: w.strip_key,
                before: b.group_key,
                workspace: .none,
                display: .none,
            )
        } else {
            session.move_tab(
                tab: w.key,
                strip: w.strip_key,
                before: .none,
                workspace: .none,
                display: .none,
            )
        }
    }
}

/// The share of its strip a wider window takes: the next preset up, and the widest there is.
fn wider_than(permille: Int) -> Int {
    if permille < 333 {
        return 333
    }
    if permille < 667 {
        return 667
    }
    return 1000
}

/// The share of its strip a narrower window takes: the next preset down, and the narrowest there
/// is.
fn narrower_than(permille: Int) -> Int {
    if permille > 667 {
        return 667
    }
    return 333
}

/// The layout strip `w`'s window stands in: the resource the strip's own actions (resize,
/// maximize) are addressed on, as the layout's place records it.
fn layout_strip(w: device.Window) -> Text {
    if let p = layout.places.find(p -> p.key == w.key) {
        return p.strip
    }
    return w.strip_key
}

/// The strip `w` stands on, as the layout publishes it.
fn strip_of(w: device.Window) -> Option<LayoutStrip> {
    let key = layout_strip(w)
    return layout.strips.find(s -> s.key == key)
}

/// The share of its row the window `w`'s strip keeps, in per-mille; the usual share while the
/// layout names none.
fn strip_width(w: device.Window) -> Int {
    if let s = strip_of(w) {
        if s.width > 0 {
            return s.width
        }
    }
    return 667
}

/// Whether the window `w`'s strip fills the shell alone.
fn strip_maximized(w: device.Window) -> Bool {
    if let s = strip_of(w) {
        return s.maximized
    }
    return false
}

/// The row (the band across the shell) the window `w`'s strip stands in.
fn row_of(w: device.Window) -> Text {
    if let s = strip_of(w) {
        return s.row
    }
    return ""
}

/// Give every window of the row the same share of it: what "every window the same" does where a
/// person sees their windows side by side in a band.
action equal_widths(w: device.Window (label: "The window")) (
    icon: .columns_2,
    label: "Equal widths",
) {
    let strips = layout.strips.filter(s -> s.row == row_of(w))
    if strips.len() > 0 {
        let share = 1000 / strips.len()
        for s in strips {
            session.resize(strip: the_strip(s.key), permille: share)
        }
    }
}

/// How many tabs the window `w`'s group holds: a window of several offers its tab's close and a
/// split, one of a single tab does not.
fn tabs_in_group(w: device.Window) -> Int {
    return device.windows.all.filter(v -> v.group_key == w.group_key).len()
}

/// Whether the window `w`'s place is pinned.
fn pinned(w: device.Window) -> Bool {
    if let p = layout.places.find(p -> p.key == w.key) {
        return p.pinned
    }
    return false
}

/// The name of the window in front of the strip `key`, as a menu offering to carry a window there
/// names it: the place's own name, or its app's where it has none, never a place number.
fn menu_strip_name(key: Text) -> Text {
    if let w = show_window_of_strip(key) {
        if let p = layout.places.find(p -> p.key == w.key) {
            if p.name != "" {
                return p.name
            }
        }
        if w.title != "" {
            return w.title
        }
        return w.origin_app
    }
    return "A window"
}

/// The window in front of the layout strip `key`: the first place of it, as its window reads.
fn show_window_of_strip(key: Text) -> Option<device.Window> {
    for p in layout.places.filter(p -> p.strip == key) {
        if let w = device.windows.all.find(w -> w.key == p.key) {
            return w
        }
    }
    return .none
}

/// Where the person is among the strips, and the way to the one beside: the shell's reading of
/// its own strips, as the strip ribbon and rail report them. The strip shown stands numbered
/// against the workspace's count, with the strip before it peeking at the leading edge — a press
/// there goes to it — and "New strip" at the trailing edge, so the rail is both the place and
/// the way to make another.
view strip_rail(a: Layout) {
    let workspace = shown_workspace(a)
    if let held = workspace {
        let rows = rows_of(a, held.key)
        if let current = rows.find(r -> r.current) {
            let at = rows.index_of(current) ?? 0
            let number = at + 1
            if let before = rows.at(at - 1) {
                stack(at: .leading, fill, align: .center, padding: .tight) {
                    ui.Card {
                        ui.Button(before.name, icon: .chevron_left, role: .plain) {
                            on press: session.show_strip(workspace: held.key, strip: before.key)
                        }
                    }
                }
            }
            stack(at: .trailing, fill, align: .center, padding: .tight) {
                ui.Card {
                    stack(gap: .tight, align: .center) {
                        ui.Text(
                            "{number} of {rows.len()}",
                            role: .caption,
                            secondary: true,
                            numeric: true,
                        )
                        ui.Button("New strip", icon: .plus, icon_only: true, role: .plain) {
                            on press: session.new_strip()
                        }
                    }
                }
            }
        }
    }
}
