/// The launcher and command palette: search and launching over everything the person can
/// reach, typed at once. It is the way in without a window: they type, the first row opens on
/// Enter.
///
/// The panel is the floating material, because it covers what the person is looking at and
/// they choose from it: the field in its header, the results in its body, the keys under it.
/// The results are the installed applications, the layout's windows and the shell's own surfaces; what
/// apps, files and actions hold beyond those is the platform's search, which Enter hands
/// `submit_search` to, so no list this view keeps can go stale.
///
/// "Open as a tab" is the other way out of a search, beside Enter's: what the text finds joins
/// the strip in front as its tab (shell.md §2, §3), one arranging action of the person's, so
/// the shell fills with the person's work rather than a second strip for every result.
/// The launcher, with the person's last query in the field.
view launcher() {
    state query: Text = device.overlays.spotlight
    let typed = query != ""
    let front = front_strip()
    // The applications installed here, in the person's language, narrowed by what they type: the
    // name or the package id holding the text, so typing "no" finds Notes by its name as well as
    // "so.tilo.notes" by the id the agent names it with. With nothing typed every app is offered,
    // so the launcher is the way in to all of them without a search.
    let apps = apps_matching(query)
    let windows = if typed {
        device.windows.all.matching(query).sort_by(w -> w.recency).take(10)
    } else {
        device.windows.all.sort_by(w -> w.recency).take(8)
    }
    ui.FloatingPanel {
        header: {
            ui.TextInput(bind: query, placeholder: "Search apps, windows, actions and files") {
                on submit: open_first(query)
            }
        }
        body: {
            // The apps first: the launcher's one job is to open one, and what a person types is
            // most often an app's name. The first row is what Enter opens; a press on any row
            // opens it, as a press would.
            if not apps.is_empty() {
                ui.Section(if typed { "Apps" } else { "All apps" }) {
                    for a in apps {
                        app_row(a)
                    }
                }
            }
            if not windows.is_empty() {
                ui.Section(if typed { "Windows" } else { "Recent windows" }) {
                    for w in windows {
                        window_row(w)
                    }
                }
            }
            if typed and apps.is_empty() and windows.is_empty() {
                ui.EmptyState(
                    "Nothing here matches",
                    message: "Search everywhere for it",
                    icon: .search,
                ) {
                    on act: session.search(query: query)
                }
            }
            if not typed {
                shell_surfaces()
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

/// The installed apps whose name or package id holds what the person typed, in name order: the
/// one list the field filters, so typing narrows the apps as it narrows the windows. A typed
/// query that matches nothing leaves the list empty, which is what the empty state says.
fn apps_matching(query: Text) -> List<device.App> {
    let needle = query.trim().lower()
    if needle == "" {
        return device.apps.installed.sort_by(a -> a.name)
    }
    device.apps.installed
        .filter(a -> a.name.lower().contains(needle) or a.key.lower().contains(needle))
        .sort_by(a -> a.name)
}

/// Enter's work on a query: the first app the text finds opens, so typing "no" and pressing Enter
/// opens Notes; a query that names no app is what search everywhere answers, as Enter has always
/// done for what the shell's own rows do not hold.
action open_first(query: Text) {
    if let a = apps_matching(query).first() {
        session.open_app(app: a.key)
    } else { session.search(query: query) }
}

/// One installed app: its name, its package id (what typing finds it by besides the name), and the
/// glyph it shows. A press opens it,
/// through the app's own `open` the dock's press and an agent's step run, so the launcher opens
/// what it lists the one way.
view app_row(a: device.App) {
    ui.ListRow(a.name, secondary: a.key, icon: a.icon) {
        on activate: session.open_app(app: a.key)
    }
}

/// The key of the strip in front: the one the person brought forward, or else the layout's
/// last, so a found resource joins the window they are working in. Empty while the layout has
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
view window_row(w: device.Window) {
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

/// The shell's own surfaces, as rows that open them: each surface, and the ways to make and join
/// a workspace.
view shell_surfaces() {
    ui.Section("The shell") {
        ui.ListRow("Overview", icon: .layout_grid) {
            on activate: device.show_surface(device.overlays, surface: "overview")
        }
        ui.ListRow("Workspaces", icon: .layers) {
            on activate: device.show_surface(device.overlays, surface: "workspaces")
        }
        ui.ListRow("Notifications", icon: .bell) {
            on activate: device.show_surface(device.overlays, surface: "centre")
        }
    }
}
