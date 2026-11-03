/// The overview: what the desk holds, to search and move things in, over the windows it
/// covers. It reads the desk the session publishes (`session.desk`, and its windows through
/// `session.window`), never a copy, and every choice it offers is an action the session publishes
/// (`so.tilo.session`), so the arrangement stays the session's: each change is attributed and
/// undoable.
///
/// The picture is the desk's: each strip top to bottom, its windows left to right at the share
/// of the strip's width they keep on the desk, a group's other tabs as narrower slices beside
/// the tab in front, under the strip's name. Each place shows the window's own live picture
/// (`ui.WindowPreview`), the compositor's, and names it below; it is a surface of the desk: the
/// desk's bar stays above it, so it holds no sidebar of its own.
view overview(d: Desk) {
    lens query: Text = d.spotlight
    let windows = d.windows.sort_by(w -> w.place)
    let here = windows.filter(w -> w.workspace_key == front_workspace(d))
    stack(gap: .section) {
        row(gap: .related, align: .center, wrap: true) {
            ui.Text("Overview", role: .heading)
            ui.Text(d.workspace, role: .caption, secondary: true)
            spacer
            ui.Button("New workspace", icon: .plus, size: .small) {
                on press: session.new_workspace()
            }
            ui.Button("Join a workspace", icon: .users, size: .small) {
                on press: session.join_workspace()
            }
        }
        ui.TextInput(value: query, placeholder: "Search windows across your workspaces") {
            on submit: session.search(query: .some(query))
        }
        rooms_here(d)
        if query == "" {
            if here.is_empty() {
                ui.EmptyState("No windows", message: "Open something to see its window", icon: .app_window, inline: true)
            }
            for s in window_strips(here) {
                picture(d, here, s)
            }
        } else {
            found(d, windows.matching(query))
        }
    }
}

/// The workspaces the person is in, as chips that switch to them, and how many people are here.
view rooms_here(d: Desk) {
    if not d.rooms.is_empty() or not d.people.is_empty() {
        row(gap: .related, align: .center, wrap: true) {
            for r in d.rooms.sort_by(r -> r.name) {
                ui.Chip(r.name, icon: if r.current { .layers } else { .folder }, selected: .some(r.current)) {
                    on press: device.switch_workspace(device.desk, workspace: r.key)
                }
            }
            if not d.people.is_empty() {
                ui.Facts {
                    ui.Fact("{d.people.len()} here", icon: .users)
                }
            }
        }
    }
}

/// One strip: its windows as places leading to trailing, each at its share of the strip's
/// width, opening what it shows and carrying its menu of moves.
view picture(d: Desk, windows: List<Win>, strip: Text) {
    ui.Section(strip_name(d, strip)) {
        row(gap: .related, align: .start) {
            for w in windows.filter(v -> v.strip_key == strip and v.tab_index == 0).sort_by(v -> v.place) {
                window_group(d, windows, w)
            }
        }
    }
}

/// One window of the desk with the tabs behind it: the tab in front at the window's share of
/// the strip, each tab behind it a narrower slice beside it, as the window stands on the desk.
view window_group(d: Desk, windows: List<Win>, w: Win) {
    let behind = behind_tabs(windows, w)
    row(fill(group_weight(w, behind)), gap: .tight, align: .start) {
        window_place(d, w, 1.0)
        for v in behind {
            // A tab behind stands 0.75 as wide as its picture is tall, and a picture is 1.6
            // times as wide as it is tall: beside the front tab, that is 0.469 of its width.
            window_place(d, v, 0.469)
        }
    }
}

/// The tabs standing behind `w` on the desk, in their order.
fn behind_tabs(windows: List<Win>, w: Win) -> List<Win> {
    return windows.filter(v -> v.group_key == w.group_key and v.tab_index > 0).sort_by(v -> v.tab_index)
}

/// How much of the strip a window with `behind` tabs behind it takes: the window's share, and
/// the slice each tab behind it stands in.
fn group_weight(w: Win, behind: List<Win>) -> Float {
    return w.share * (1.0 + 0.469 * behind.len().to_float())
}

/// One window's place: its live picture, the app it shows and the device it runs on when that is
/// not this one, lifted while it has focus, with a menu that moves it to another strip or
/// workspace, closes it, or asks about it. Its card takes `weight` of the strip's width, so a
/// place stands as wide as the window stands on the desk, up to the width a place shows at on
/// this screen: past that the picture would stand taller than the page it is on, so a window
/// wider than the screen allows is pictured no wider than that.
view window_place(d: Desk, w: Win, weight: Float) {
    let cap = if space.compact { 16rem } else if space.width_class == .expanded or space.width_class == .wide { 26rem } else { 20rem }
    ui.Card(item: w, title: w.title, selected: w.focused, fill(weight), max: cap) {
        stack(gap: .tight) {
            layers(aspect: 1.6) {
                window_picture(w)
            }
            ui.Text(w.title, role: .label, lines: 1, truncate)
            row(gap: .tight, align: .center) {
                ui.Text(app_line(w), role: .caption, secondary: true, lines: 1, truncate)
                spacer
                window_menu(d, w)
            }
        }
        on activate: session.focus(window(w))
    }
}

/// The app a window shows, and the device it runs on when it is not this one: a window on the
/// person's other device is named with it until it can be pictured live.
fn app_line(w: Win) -> Text {
    if w.device_name != "" {
        return "{w.origin_app} · {w.device_name}"
    }
    return w.origin_app
}

/// A window's commands, where its place shows them: going there, carrying it to another strip
/// or workspace, asking about it, and closing it.
view window_menu(d: Desk, w: Win) {
    ui.MenuButton("This window", icon: .more_horizontal, icon_only: true, role: .plain) {
        ui.MenuItem("Go there", icon: .arrow_right) {
            on choose: session.focus(window(w))
        }
        for s in strip_keys(d, w.workspace_key) {
            if s != w.strip_key {
                ui.MenuItem("Move to {strip_name(d, s)}", icon: .arrow_right) {
                    on choose: session.move_tab(tab: w.key, strip: .some(s), workspace: .none, display: .none)
                }
            }
        }
        for r in d.rooms.filter(r -> r.key != w.workspace_key) {
            ui.MenuItem("Move to {r.name}", icon: .layers) {
                on choose: session.move_tab(tab: w.key, strip: .none, workspace: .some(r.key), display: .none)
            }
        }
        ui.MenuItem("Close", icon: .x, destructive: true) {
            on choose: session.close_tab(window(w))
        }
    }
}

/// What a search finds among the person's windows, in every workspace: each one, going there
/// or moving it here. What apps hold is the platform search's, not the overview's.
view found(d: Desk, elsewhere: List<Win>) {
    if elsewhere.is_empty() {
        ui.EmptyState("Nothing matches", message: "Finds windows in every workspace. What is inside an app is in Search", icon: .search, inline: true)
    } else {
        ui.Section("Windows") {
            for w in elsewhere.take(16) {
                window_line(d, w)
            }
        }
    }
}

/// One window a search found, as a row that opens there, with the way to bring it here.
view window_line(d: Desk, w: Win) {
    ui.ListRow(w.title, secondary: .some(app_line(w)), meta: .some(w.workspace), item: w, icon: if w.preview == "withheld" { .eye_off } else { .app_window }) {
        trailing: {
            ui.MenuButton("This window", icon: .more_horizontal, icon_only: true, role: .plain) {
                ui.MenuItem("Go there", icon: .arrow_right) {
                    on choose: session.focus(window(w))
                }
                if w.workspace_key != front_workspace(d) {
                    ui.MenuItem("Move here", icon: .arrow_down) {
                        on choose: session.move_tab(tab: w.key, strip: .none, workspace: .some(front_workspace(d)), display: .none)
                    }
                }
            }
        }
        on activate: session.focus(window(w))
    }
}

/// The strips a list of windows stands on, top to bottom.
fn window_strips(windows: List<Win>) -> List<Text> {
    let in_order = windows.sort_by(w -> w.place).map(w -> w.strip_key).filter(s -> s != "")
    return in_order.group(s -> s).map(p -> p.key)
}

/// The ids of the strips of the workspace `workspace`, top to bottom, as the desk's windows
/// name them.
fn strip_keys(d: Desk, workspace: Text) -> List<Text> {
    let mine = d.windows.filter(w -> w.workspace_key == workspace)
    let keys = mine.map(w -> w.strip_key).filter(s -> s != "")
    return keys.group(s -> s).map(p -> p.key)
}

/// The name of the strip `key`: its own, or the app of the first window on it when it has none,
/// so no strip is named by where it stands.
fn strip_name(d: Desk, key: Text) -> Text {
    if let w = d.windows.filter(w -> w.strip_key == key).sort_by(v -> v.place).first() {
        if w.strip != "" {
            return w.strip
        }
        return w.origin_app
    }
    return ""
}
