/// The history surface: what the person opened lately, read off the workspace's history of what
/// its windows showed (`layout.recents`). It is the system's, so no app keeps a recents list of
/// its own: an app's recents button asks for this surface narrowed to it
/// (`session.recents(app: "so.tilo.tube")`, or `kind:` for one type of its), as its search
/// button asks for search scoped to it, and the session says what it was opened on
/// (`device.overlays.history_app`, `history_kind`). Opened on nothing, it lists every app's.
///
/// Each resource stands once, at its latest open: what the person opened themselves first, then
/// what others in the workspace opened, newest first. A press opens it again, through the
/// layout's own `open`. Clear hides what is listed from this person's list alone, of the scope it
/// shows: the workspace's history keeps every open, and nobody else's list changes.
view history() {
    let package = device.overlays.history_app
    let kind = device.overlays.history_kind
    let shown = recents_in(package, kind)
    row(fill, distribute: .center, padding: .related) {
        ui.FloatingPanel(
            width: if not space.compact { 480.0 },
            fill: if space.compact { 1.0 } else { 0.0 },
        ) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text(history_title(package, kind, shown), role: .heading)
                    spacer
                    if not shown.is_empty() {
                        ui.Button("Clear", icon: .eraser, size: .small) {
                            on press: session.clear_recents(
                                app: if package != "" { package },
                                kind: if kind != "" { kind },
                            )
                        }
                    }
                }
            }
            body: {
                if shown.is_empty() {
                    ui.EmptyState(
                        "Nothing opened lately",
                        message: "What you open shows up here",
                        icon: .history,
                    )
                } else if package != "" or kind != "" {
                    scroll {
                        for r in shown {
                            recent_row(r)
                        }
                    }
                } else {
                    scroll {
                        for a in apps_of(shown) {
                            ui.Section(a) {
                                for r in shown.filter(r -> r.app_title == a) {
                                    recent_row(r)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The person's recents of `package` (an app's id) or of `kind` (a type as its app publishes
/// it), as the session lists them; both empty is every app's.
fn recents_in(package: Text, kind: Text) -> List<LayoutRecent> {
    let all = layout.recents
    if package != "" {
        return all.filter(r -> r.origin_app == package)
    }
    if kind != "" {
        return all.filter(r -> r.kind == kind)
    }
    all
}

/// What the surface is called: the app's name when it shows one app's recents.
fn history_title(package: Text, kind: Text, shown: List<LayoutRecent>) -> Text {
    if package != "" {
        if let first = shown.first() {
            return "Recent in {first.app_title}"
        }
    }
    "Recent"
}

/// The apps `shown` holds resources of, by name, each once, in the order they first appear.
fn apps_of(shown: List<LayoutRecent>) -> List<Text> {
    shown.map(r -> r.app_title).group(a -> a).map(p -> p.key)
}

/// One recent resource: what it is, who opened it last and when. Its app is the section it
/// stands under, or the surface's title when it shows one app's.
view recent_row(r: LayoutRecent) {
    ui.ListRow(r.title, secondary: opened_by(r), item: r, icon: .history) {
        trailing: {
            ui.Time(r.at, role: .caption, past: true, secondary: true)
        }
        on activate: open_recent(r.resource)
    }
}

/// Who opened it last, as the row says it.
fn opened_by(r: LayoutRecent) -> Text {
    if r.mine { "Opened by you" } else { "Opened by {r.by}" }
}

/// Opens the recent resource `address` again, beside the window in front, and puts the surface
/// away: the same `open` an agent and a reference's press run.
action open_recent(address: Text) (label: "Open") {
    if let found = resource(address) { session.open(resource: found) }
    device.show_surface(device.overlays, surface: "none")
}
