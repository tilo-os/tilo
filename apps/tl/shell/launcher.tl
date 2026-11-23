/// The launcher and command palette: search and launching over everything the person can
/// reach, typed at once. It is the way in without a window: they type, the arrows walk the
/// rows, and Enter opens the one they reached.
///
/// The panel is the floating material, because it covers what the person is looking at and
/// they choose from it. Its field is a `ui.Autocomplete`, so the arrows walk the rows, Enter
/// takes the row they reached, Tab and Shift+Tab move between groups, and Escape leaves; the
/// rows scroll with the selection, so a search of ten thousand answers shows the one walked to.
///
/// What the rows are comes from two places, as a person expects. The installed applications and
/// the layout's windows are here, narrowing as they type, so the common case answers on the
/// keystroke. Everything beyond them — files, actions, people, Settings' pages, what apps hold —
/// is the platform's search (`device.search`), which "Search everywhere" runs: it answers with
/// its own rows, under the groups its kinds name, each carrying its glyph and what says what it
/// is. Choosing one runs it through the shell's own `choose-search-row`, so a press, a key and an
/// agent reach the same thing.
///
/// "Open as a tab" is the other way out of a search, beside Enter's: what the text finds joins
/// the strip in front as its tab (shell.md §2, §3), one arranging action of the person's, so
/// the shell fills with the person's work rather than a second strip for every result.
/// The launcher, with the person's last query in the field.
view launcher() {
    state query: Text = device.overlays.spotlight
    let search = device.search
    let typed = query.trim() != ""
    // The search's rows are for what is typed now, not for an earlier query: a query typed on
    // since narrows back to the apps and windows here until "Search everywhere" runs again.
    let current = typed and search.open and search.query.trim().lower() == query.trim().lower()
    ui.FloatingPanel {
        header: {
            ui.Autocomplete(
                bind: query,
                placeholder: "Search apps, windows, actions and files",
                autofocus: true,
                open_list: true,
                tab_groups: true,
                filter: false,
                max_visible: 10,
                status: launcher_status(search, typed, current),
            ) {
                on accept(choice): choose_launcher_row(choice, current)
                on cancel: close_launcher()
                if current and not search.groups.is_empty() {
                    for group in search.groups {
                        ui.Suggestion("group:{group.kind}", label: group.kind, header: true)
                        for row in group.rows {
                            ui.Suggestion(
                                "row:{row.index.to_text()}",
                                label: row.title,
                                subtitle: if row.subtitle != "" {
                                    row.subtitle
                                } else { row.facts.at(0) ?? "" },
                                icon: group.icon,
                            )
                        }
                    }
                } else {
                    if not apps_matching(query).is_empty() {
                        ui.Suggestion(
                            "group:apps",
                            label: if typed { "Apps" } else { "All apps" },
                            header: true,
                        )
                        for a in apps_matching(query).take(12) {
                            ui.Suggestion(
                                "app:{a.key}",
                                label: a.name,
                                subtitle: a.key,
                                icon: a.icon,
                            )
                        }
                    }
                    if not launcher_windows(query, typed).is_empty() {
                        ui.Suggestion(
                            "group:windows",
                            label: if typed { "Windows" } else { "Recent windows" },
                            header: true,
                        )
                        for w in launcher_windows(query, typed) {
                            ui.Suggestion(
                                "win:{w.key}",
                                label: w.title,
                                subtitle: w.origin_app,
                                icon: if w.preview == "withheld" { .eye_off } else { .app_window },
                            )
                        }
                    }
                    if not typed {
                        if not device.launchpad.cards.is_empty() {
                            ui.Suggestion("group:welcome", label: "Welcome", header: true)
                            for card in device.launchpad.cards {
                                ui.Suggestion(
                                    "card:{card.slug}",
                                    label: card.label,
                                    icon: card.icon,
                                )
                            }
                        }
                        ui.Suggestion("group:shell", label: "The shell", header: true)
                        ui.Suggestion("do:overview", label: "Overview", icon: .layout_grid)
                        ui.Suggestion("do:workspaces", label: "Workspaces", icon: .layers)
                        ui.Suggestion("do:notifications", label: "Notifications", icon: .bell)
                    }
                }
            }
        }
        footer: {
            row(gap: .related, align: .center, wrap: true) {
                if typed and front_strip() != "" {
                    ui.Button("Open as a tab", icon: .columns_2, role: .primary, size: .small) {
                        on press: session.place_found(
                            text: query,
                            view: .none,
                            strip: front_strip(),
                            x: .none,
                            y: .none,
                        )
                    }
                }
                ui.Button(
                    "Open",
                    icon: .arrow_right,
                    icon_only: true,
                    role: .primary,
                    size: .small,
                    key: "Enter",
                ) {
                    on press: choose_launcher_row(query, current)
                }
                spacer
                key_hints_launcher()
            }
        }
    }
}

/// The layout's windows whose name holds what the person typed (or the recent ones, with nothing
/// typed): the instant half of the launcher.
fn launcher_windows(query: Text, typed: Bool) -> List<device.Window> {
    if typed {
        return device.windows.all.matching(query).sort_by(w -> w.recency).take(8)
    }
    return device.windows.all.sort_by(w -> w.recency).take(6)
}

/// What the field says under itself while it has nothing to show: that the search still fills
/// in, or that nothing matched.
fn launcher_status(search: device.Search, typed: Bool, current: Bool) -> Text {
    if not typed {
        return ""
    }
    if current and search.searching {
        return "Searching…"
    }
    if current and search.groups.is_empty() {
        return "Nothing matches {search.query}"
    }
    return ""
}

/// The installed apps whose name or package id holds what the person typed, in name order: the
/// one list the field filters, so typing narrows the apps as it narrows the windows. A typed
/// query that matches nothing leaves the list empty.
fn apps_matching(query: Text) -> List<device.App> {
    let needle = query.trim().lower()
    if needle == "" {
        return device.apps.installed.sort_by(a -> a.name)
    }
    device.apps.installed
        .filter(a -> a.name.lower().contains(needle) or a.key.lower().contains(needle))
        .sort_by(a -> a.name)
}

/// What choosing the row the person reached does, read off the row's value: opening an
/// application or a window here, running a row the search found, or one of the shell's own
/// surfaces. A row is only ever one of those, so a raw choice — what is typed with no row
/// under it — opens the first row the text finds, or searches everywhere for it.
action choose_launcher_row(choice: Text, current: Bool) {
    let kind = choice.split(":").at(0) ?? ""
    let argument = choice.split(":").at(1) ?? ""
    if kind == "app" {
        session.open_app(app: argument)
        close_launcher()
    } else if kind == "win" {
        if let w = device.windows.all.find(w -> w.key == argument) {
            session.focus(window(w))
            close_launcher()
        }
    } else if kind == "row" {
        if let at = argument.to_int() {
            // The row is the search's own, by the place it stands in: run it the way the Rust
            // palette does, then leave the launcher for what it opened.
            session.choose_search_row(row: at)
            close_launcher()
        }
    } else if kind == "card" {
        // A welcome card the empty launcher offers: the shell's own launchpad verb, so a new
        // workspace's first step is one published action.
        session.launchpad(card: argument)
        close_launcher()
    } else if kind == "do" {
        if argument == "overview" {
            open_surface("overview")
        } else if argument == "workspaces" {
            open_surface("workspaces")
        } else if argument == "notifications" { open_surface("centre") }
    } else if kind != "group" {
        // What was typed, with no row under it: the search's own first row when its rows are for
        // this query, else the first application or window it names, else the platform's search
        // for everything the shell does not hold.
        if current and not device.search.groups.is_empty() {
            // Enter opens what a person most likely typed: the app named, when the search found
            // one, else the row the search put first.
            let group = device.search.groups.find(g -> g.kind == "Apps")
                ?? device.search.groups.first()
            if let found = group {
                if let row = found.rows.first() {
                    session.choose_search_row(row: row.index)
                    close_launcher()
                }
            }
        } else if let a = apps_matching(choice).first() {
            session.open_app(app: a.key)
            close_launcher()
        } else if let w = device.windows.all.matching(choice).sort_by(w -> w.recency).first() {
            session.focus(window(w))
            close_launcher()
        } else { session.search(query: choice) }
    }
}

/// Open the shell's surface `surface` (`overview`, `workspaces`, `centre`), as the bar's own
/// controls do.
action open_surface(surface: Text) {
    device.show_surface(device.overlays, surface: surface)
}

/// Leave the launcher: the surface stack goes back to the shell at rest.
action close_launcher() {
    device.show_surface(device.overlays, surface: "none")
}

/// The key of the strip in front: the one whose window has the keyboard, or else the layout's
/// last, so a found resource joins the window they are working in. Empty while the layout has
/// no strip.
fn front_strip() -> Text {
    let a = layout
    if a.front != "" {
        return a.front
    }
    if let last = a.strips.last() {
        return last.key
    }
    return ""
}
