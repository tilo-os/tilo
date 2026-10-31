/// The launcher and command palette: search and launching over everything the person can
/// reach, typed at once. It is the way in without a window: they type, the first row opens on
/// Enter.
///
/// The panel is the floating material, because it covers what the person is looking at and
/// they choose from it: the field in its header, the results in its body, the keys under it.
/// The results are the desk's windows and the desk's own doors; what apps, files and actions
/// hold is the platform's search, which Enter hands `submit_search` to, so no list this view
/// keeps can go stale.

/// The launcher, with the person's last query in the field.
view launcher(d: Desk) {
    lens query: Text = d.spotlight
    let typed = query != ""
    let windows = if typed { d.windows.matching(query).sort_by(w -> w.recency).take(10) } else { d.windows.sort_by(w -> w.recency).take(8) }
    ui.FloatingPanel {
        header: {
            ui.TextInput(value: query, placeholder: "Search apps, windows, actions and files") {
                on submit: submit_search(query)
            }
        }
        body: {
            if not windows.is_empty() {
                ui.Section(if typed { "Windows" } else { "Recent windows" }) {
                    for w in windows {
                        window_row(w)
                    }
                }
            }
            if typed and windows.is_empty() {
                ui.EmptyState("Nothing here matches", message: "Search everywhere for it", icon: .search) {
                    on act: submit_search(query)
                }
            }
            if not typed {
                desk_doors()
            }
        }
        footer: {
            row(gap: .related, align: .center, wrap: true) {
                if typed {
                    ui.Button("Search everywhere for {query}", icon: .search, role: .primary, size: .small) {
                        on press: submit_search(query)
                    }
                }
                spacer
                key_hints_launcher()
            }
        }
    }
}

/// One window the search found: what it is, what it shows, and where it is.
view window_row(w: Win) {
    ui.ListRow(w.title, secondary: .some(w.origin_app), meta: .some(if w.workspace != "" { w.workspace } else { w.strip }), item: w, icon: if w.preview == "withheld" { .eye_off } else { .app_window }) {
        on activate: focus_window(w)
    }
}

/// The desk's own doors, as rows that open them: each surface, and the ways to make and join a
/// workspace.
view desk_doors() {
    ui.Section("The desk") {
        ui.ListRow("Overview", secondary: .some("Every strip and window, to search and move"), icon: .layout_grid) {
            on activate: show_surface("overview")
        }
        ui.ListRow("Workspaces", secondary: .some("Who is in each, and what it holds"), icon: .layers) {
            on activate: show_surface("workspaces")
        }
        ui.ListRow("Notifications", secondary: .some("What apps and agents told you"), icon: .bell) {
            on activate: show_surface("centre")
        }
        ui.ListRow("Ask an agent", secondary: .some("Run a task over what you point at"), icon: .bot) {
            on activate: show_surface("composer")
        }
    }
}
