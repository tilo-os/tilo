/// The notifications: the banner that arrives, and the centre that keeps them. Both are views
/// of the centre the session publishes (`device.notifications`), grouped by what sent them: an
/// app, or the agent task that did. Each notice carries its sender's own actions, in the order
/// its sender put them (`device.act`).
///
/// A banner is the centre's newest notice, lifted out of it; a group is an app's or a task's
/// notices together. Nothing here decides what a notice means: its kind, its sender and its
/// actions are the sender's. It is a surface over the windows, so it holds no sidebar of its own.
///
/// The notification centre is a floating panel at the screen's trailing edge, over what it
/// covers, on the theme's material as every notification centre is: its header says how many
/// are waiting and holds the way to clear them all, and each sender's group opens as one on the
/// toolkit's disclosure motion when it arrives.
view notices() {
    state query: Text = ""
    let all = device.notifications.notices
    let shown = all.matching(query)
    row(fill, distribute: .end, padding: .related) {
        ui.FloatingPanel(
            width: if not space.compact { 360.0 },
            fill: if space.compact { 1.0 } else { 0.0 },
        ) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("Notifications", role: .heading)
                    ui.Text(unread_line(), role: .caption, secondary: true)
                    spacer
                    if not all.is_empty() {
                        ui.Button("Clear all", icon: .trash, size: .small) {
                            on press: device.clear(device.notifications)
                        }
                    }
                }
            }
            body: {
                if all.is_empty() {
                    ui.EmptyState("Nothing to catch up on", icon: .bell)
                } else {
                    ui.TextInput(bind: query, placeholder: "Search notifications")
                    if shown.is_empty() {
                        ui.EmptyState(
                            "Nothing matches that",
                            message: "Clear the search to see every notice",
                            icon: .search,
                            inline: true,
                        )
                    }
                    scroll {
                        for g in groups_of(shown) {
                            ui.DisclosureBand {
                                notice_group(g, shown.filter(n -> n.from == g))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// How many of the notices the person was told they have not seen: the count the bar's bell and
/// the centre's header both read.
fn unread() -> Int {
    return device.notifications.notices.count(n -> not n.seen)
}

/// Two notices a sender left and the person has not seen: what a screen shows before the
/// person has caught up, so the bar's bell has something to count.
fn waiting() -> device.Notifications {
    return device.Notifications(notices: [mia_mention(), threads_filed()])
}

/// A notice: Mia asking about the launch date.
fn mia_mention() -> device.Notice {
    return device.Notice(
        number: 1,
        title: "Mia mentioned you in Release planning",
        body: "Can you check the launch date?",
        from: "Notes",
        kind: "info",
        seen: false,
        actions: [
            device.NoticeAction(key: "open", label: "Reply"),
            device.NoticeAction(key: "later", label: "Later"),
        ],
    )
}

/// A notice: Fieldwork done filing.
fn threads_filed() -> device.Notice {
    return device.Notice(
        number: 2,
        title: "Three threads filed",
        body: "Fieldwork filed 3 threads into Receipts.",
        from: "Fieldwork",
        kind: "success",
        seen: false,
    )
}

/// What the centre says of them: how many are waiting, or that none is.
fn unread_line() -> Text {
    let waiting = unread()
    if waiting == 0 { "All read" } else if waiting == 1 { "1 unread" } else { "{waiting} unread" }
}

/// One sender's notices, under its name: each with its title, what it says and its actions.
view notice_group(sender: Text, notices: List<device.Notice>) {
    ui.Section(sender) {
        for n in notices {
            notice_row(n)
        }
    }
}

/// One notice: its title, what it says, its sender, its actions and the way to dismiss it. An
/// unread one is lifted and bears a dot in the person's colour, toned by its kind, so a failure
/// reads as one at a glance.
view notice_row(n: device.Notice) {
    ui.ListRow(n.title, secondary: n.body, meta: n.from, selected: not n.seen) {
        trailing: {
            if n.kind == "error" {
                ui.Badge(dot: not n.seen, tone: .danger)
            } else if n.kind == "warning" {
                ui.Badge(dot: not n.seen, tone: .warning)
            } else if n.kind == "success" {
                ui.Badge(dot: not n.seen, tone: .success)
            } else {
                ui.Badge(dot: not n.seen)
            }
        }
    }
    row(gap: .related, align: .center, wrap: true) {
        for a in n.actions {
            ui.Button(a.label, size: .small) {
                on press: device.act(device.notifications, notification: n.number, action: a.key)
            }
        }
        ui.Button("Dismiss", icon: .x, icon_only: true, size: .small, role: .plain) {
            on press: device.dismiss(device.notifications, notification: n.number)
        }
    }
}

/// A banner: the newest notice, risen over the windows, with its first action and a dismiss. The
/// session takes it away when it is dismissed or its sender closes it, and names the one it has
/// up (`banner`), so the banner and the centre never disagree about what just arrived.
/// What the person was told is held back while notifications are silenced.
///
/// A wide screen shows the toolkit's banner; a narrow one shows the notice as a row with its
/// actions under it, so nothing is cut at any text size.
view banner() {
    let centre = device.notifications
    if let n = centre.notices.find(n -> n.number == (centre.banner ?? -1)) {
        if (centre.quiet ?? "") == "" {
            if space.compact or space.width < 44.0 {
                banner_row(n)
            } else {
                banner_full(n)
            }
        }
    }
}

/// A banner on a wide screen: the notice with its kind's tone, what sent it, its first
/// action. Each part it has is the part it shows: a notice that says nothing puts no line under
/// the title, and one with no sender names none above it.
view banner_full(n: device.Notice) {
    let actions = n.actions
    ui.Banner(
        n.title,
        message: if n.body != "" { n.body },
        source: if n.from != "" { n.from },
        kind: if n.kind == "error" {
            .error
        } else if n.kind == "warning" {
            .warning
        } else if n.kind == "success" { .success } else { .info },
        action: if not actions.is_empty() { actions.first()?.label ?? "Open" },
    ) {
        on act: device.act(
            device.notifications,
            notification: n.number,
            action: actions.first()?.key ?? "",
        )
        on dismiss: device.dismiss(device.notifications, notification: n.number)
    }
}

/// A banner on a narrow screen: the notice as a row with what sent it, its actions under it.
view banner_row(n: device.Notice) {
    stack(gap: .related) {
        ui.ListRow(
            n.title,
            secondary: if n.body != "" { n.body },
            meta: if n.from != "" { n.from },
        ) {
            trailing: {
                ui.Badge(
                    dot: true,
                    tone: if n.kind == "error" {
                        .danger
                    } else if n.kind == "warning" { .warning } else { .info },
                )
            }
        }
        row(gap: .related, wrap: true) {
            for a in n.actions {
                ui.Button(a.label, size: .small) {
                    on press: device.act(
                        device.notifications,
                        notification: n.number,
                        action: a.key,
                    )
                }
            }
            ui.Button("Dismiss", icon: .x, icon_only: true, size: .small, role: .plain) {
                on press: device.dismiss(device.notifications, notification: n.number)
            }
        }
    }
}

/// What a list of notices was sent by, each once, in the order the notices came.
fn groups_of(notices: List<device.Notice>) -> List<Text> {
    return notices.map(n -> n.from).group(g -> g).map(p -> p.key)
}
