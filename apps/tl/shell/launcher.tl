/// The launcher and command palette: search and launching over everything the person can
/// reach, typed at once. It is the way in without a window: they type, the first row opens on
/// Enter.
///
/// The panel is the floating material, because it covers what the person is looking at and
/// they choose from it: the field in its header, the results in its body, the keys under it.
/// The results are the desk's windows and the desk's own doors; what apps, files and actions
/// hold is the platform's search, which Enter hands `submit_search` to, so no list this view
/// keeps can go stale.
///
/// "Open as a tab" is the other way out of a search, beside Enter's: what the text finds joins
/// the strip in front as its tab (shell.md §2, §3), one arranging action of the person's, so
/// the desk fills with the person's work rather than a second strip for every result.
/// The launcher, with the person's last query in the field.
view launcher(d: Desk) {
    state query: Text = d.spotlight
    let typed = query != ""
    let front = front_strip()
    let windows = if typed {
        d.windows.matching(query).sort_by(w -> w.recency).take(10)
    } else {
        d.windows.sort_by(w -> w.recency).take(8)
    }
    ui.FloatingPanel {
        header: {
            ui.TextInput(bind: query, placeholder: "Search apps, windows, actions and files") {
                on submit: session.search(query: query)
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
                ui.EmptyState(
                    "Nothing here matches",
                    message: "Search everywhere for it",
                    icon: .search,
                ) {
                    on act: session.search(query: query)
                }
            }
            if not typed {
                desk_doors(d)
            }
        }
        footer: {
            row(gap: .related, align: .center, wrap: true) {
                if typed {
                    if front != "" {
                        ui.Button("Open as a tab", icon: .columns_2, role: .primary, size: .small) {
                            on press: session.place_found(
                                text: query,
                                view: .none,
                                strip: front,
                                x: .none,
                                y: .none,
                            )
                        }
                    }
                    ui.Button(
                        "Search everywhere for {query}",
                        icon: .search,
                        role: if front == "" { .primary } else { .secondary },
                        size: .small,
                    ) {
                        on press: session.search(query: query)
                    }
                }
                spacer
                key_hints_launcher()
            }
        }
    }
}

/// The key of the strip in front: the one the person brought forward, or else the desk's
/// last, so a found resource joins the window they are working in. Empty while the desk has
/// no strip.
fn front_strip() -> Text {
    let a = arrangement
    let v = viewing
    if v.focused != "" {
        return v.focused
    }
    if let last = a.strips.last() {
        return last.key
    }
    return ""
}

/// One window the search found: what it is, what it shows, and where it is.
view window_row(w: Win) {
    ui.ListRow(
        w.title,
        secondary: w.origin_app,
        meta: if w.workspace != "" { w.workspace } else { w.strip },
        item: w,
        icon: if w.preview == "withheld" { .eye_off } else { .app_window },
    ) {
        on activate: session.focus(window(w))
    }
}

/// The desk's own doors, as rows that open them: each surface, and the ways to make and join a
/// workspace.
view desk_doors(d: Desk) {
    ui.Section("The desk") {
        ui.ListRow("Overview", icon: .layout_grid) {
            on activate: device.show_surface(device.desk, surface: "overview")
        }
        ui.ListRow("Workspaces", icon: .layers) {
            on activate: device.show_surface(device.desk, surface: "workspaces")
        }
        ui.ListRow("Notifications", icon: .bell) {
            on activate: device.show_surface(device.desk, surface: "centre")
        }
    }
}
