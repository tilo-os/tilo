/// The overview: what the desk holds, to search and move things in, over the windows it
/// covers. It reads the desk the session publishes (`session.desk`, and its windows through
/// `session.window`), never a copy, and every choice it offers is an action the session runs
/// (`session.run`), so the arrangement stays the session's: each change is attributed and
/// undoable.
///
/// The picture is the desk's: each strip top to bottom, the strip's windows left to right in
/// strip order, under the strip's name. A window's own picture is the host's to supply; this
/// view lays out its place, names it and marks who else is on it, and it is a surface of the
/// desk: the desk's bar stays above it, so it holds no sidebar of its own.
view overview(d: Desk) {
    lens query: Text = ""
    let windows = d.windows.sort_by(w -> w.recency)
    let here = windows.filter(w -> w.workspace == d.workspace)
    let elsewhere = windows.filter(w -> w.workspace != d.workspace).matching(query)
    stack(gap: .section) {
        row(gap: .related, align: .center, wrap: true) {
            ui.Text("Overview", role: .heading)
            ui.Text(d.workspace, role: .caption, secondary: true)
            spacer
            ui.Button("New workspace", icon: .plus, size: .small) {
                on press: new_workspace(d)
            }
            ui.Button("Join a workspace", icon: .users, size: .small) {
                on press: join_workspace(d)
            }
        }
        ui.TextInput(value: query, placeholder: "Search windows across your workspaces") {
            on submit: submit_search(d, query)
        }
        rooms_here(d)
        if query == "" {
            for s in window_strips(here) {
                picture(d, here, s)
            }
        } else {
            found(d, elsewhere)
        }
    }
}

/// The workspaces the person is in, as chips that switch to them, and how many people are here.
view rooms_here(d: Desk) {
    if not d.rooms.is_empty() or not d.people.is_empty() {
        row(gap: .related, align: .center, wrap: true) {
            for r in d.rooms.sort_by(r -> r.name) {
                ui.Chip(r.name, icon: if r.current { .layers } else { .folder }, selected: .some(r.current)) {
                    on press: switch_to_key(r)
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

/// One strip: its windows as places left to right, each opening what it shows and carrying its
/// menu of moves.
view picture(d: Desk, windows: List<Win>, strip: Text) {
    ui.Section(strip) {
        if windows.count(w -> w.strip == strip) == 0 {
            ui.EmptyState("Nothing on this strip", message: "Open something, or carry a window here", icon: .columns_2, inline: true)
        }
        grid(min: 15rem) {
            for w in windows.filter(w -> w.strip == strip) {
                window_place(d, w)
            }
        }
    }
}

/// One window's place: its picture, its title and the app it shows, lifted while it has focus,
/// with a menu that moves it to another strip or workspace, closes it, or asks about it.
view window_place(d: Desk, w: Win) {
    ui.Card(item: w, title: w.title, selected: w.focused) {
        row(gap: .related, align: .center) {
            ui.Icon(if w.preview == "withheld" { .eye_off } else { .app_window }, size: .large, secondary: true)
            stack(gap: .tight) {
                ui.Text(w.title, role: .label, lines: 1, truncate)
                ui.Text(w.origin_app, role: .caption, secondary: true, lines: 1)
            }
            window_menu(d, w)
        }
        on activate: focus_window(w)
    }
}

/// A window's commands, where its place shows them: going there, carrying it to another strip
/// or workspace, asking about it, and closing it.
view window_menu(d: Desk, w: Win) {
    ui.MenuButton("This window", icon: .more_horizontal, icon_only: true, role: .plain) {
        ui.MenuItem("Go there", icon: .arrow_right) {
            on choose: focus_window(w)
        }
        for s in strip_names(d, w.workspace) {
            if s != w.strip {
                ui.MenuItem("Move to {s}", icon: .arrow_right) {
                    on choose: move_to_strip(w, s)
                }
            }
        }
        for r in d.rooms.filter(r -> r.name != w.workspace) {
            ui.MenuItem("Move to {r.name}", icon: .layers) {
                on choose: move_to_room(w, r.name)
            }
        }
        ui.MenuItem("Ask an agent…", icon: .bot) {
            on choose: ask_about(w)
        }
        ui.MenuItem("Close", icon: .x, destructive: true) {
            on choose: close_window(w)
        }
    }
}

/// What a search finds across the person's other workspaces: each window, going there or
/// moving it here. What apps hold is the platform search's, not the overview's.
view found(d: Desk, elsewhere: List<Win>) {
    if elsewhere.is_empty() {
        ui.EmptyState("Nothing matches", message: "Windows of your other workspaces show here. What apps hold is in Search", icon: .search, inline: true)
    } else {
        ui.Section("In other workspaces") {
            for w in elsewhere.take(16) {
                window_line(d, w)
            }
        }
    }
}

/// One window of another workspace, as a row that opens there, with the way to bring it here.
view window_line(d: Desk, w: Win) {
    ui.ListRow(w.title, secondary: .some(w.origin_app), meta: .some(w.workspace), item: w, icon: if w.preview == "withheld" { .eye_off } else { .app_window }) {
        trailing: {
            ui.MenuButton("This window", icon: .more_horizontal, icon_only: true, role: .plain) {
                ui.MenuItem("Go there", icon: .arrow_right) {
                    on choose: focus_window(w)
                }
                ui.MenuItem("Move here", icon: .arrow_down) {
                    on choose: move_to_room(w, d.workspace)
                }
            }
        }
        on activate: focus_window(w)
    }
}

/// The strips a list of windows names, by name, in the order the windows stand in them.
fn window_strips(windows: List<Win>) -> List<Text> {
    let in_order = windows.sort_by(w -> w.recency).map(w -> w.strip).filter(s -> s != "")
    return in_order.group(s -> s).map(p -> p.key)
}

/// The names of the strips of `workspace`, as the desk's windows name them.
fn strip_names(d: Desk, workspace: Text) -> List<Text> {
    return window_strips(d.windows.filter(w -> w.workspace == workspace))
}
