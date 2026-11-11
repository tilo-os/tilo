/// The notifications: the banner that arrives, and the centre that keeps them. Both are views
/// of the desk's notices (`session.notice`, and each action of a notice through
/// `session.notice_action`), grouped by what sent them: an app, or the agent task that did.
/// Each notice carries its sender's own actions, in the order its sender put them, and each
/// action is the sender's, called on the desk (`device.act_on_notice`).
///
/// A banner is the centre's newest notice, lifted out of it; a group is an app's or a task's
/// notices together. Nothing here decides what a notice means: its kind, its sender and its
/// actions are the sender's. It is a surface of the desk, so it holds no sidebar of its own.
///
/// The notification centre is a floating panel at the desk's trailing edge, over what it
/// covers, on the theme's material as every notification centre is: its header says how many
/// are waiting and holds the way to clear them all, and each sender's group opens as one on the
/// toolkit's disclosure motion when it arrives.
view notices(d: Desk) {
    state query: Text = ""
    let all = d.notices.sort_by(n -> n.key)
    let shown = all.matching(query)
    row(fill, distribute: .end, padding: .related) {
        ui.FloatingPanel(
            width: if not space.compact { 360.0 },
            fill: if space.compact { 1.0 } else { 0.0 },
        ) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("Notifications", role: .heading)
                    ui.Text(unread_line(d), role: .caption, secondary: true)
                    spacer
                    if not all.is_empty() {
                        ui.Button("Clear all", icon: .trash, size: .small) {
                            on press: device.clear_notices(device.desk)
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
                                notice_group(d, g, shown.filter(n -> n.origin_app == g))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// How many of the notices the person was told they have not seen: the count the bar's bell and
/// the centre's header both read, each from the notices it shows.
fn unread(d: Desk) -> Int {
    return d.notices.count(n -> not n.seen)
}

/// Two notices a sender left and the person has not seen: what the shell stages on a desk it
/// shows before the person has caught up, so the bar's bell has something to count.
action seed_waiting(d: Desk) -> Int {
    d.notices.insert(
        Notice(
            key: "n1",
            title: "Mia mentioned you in Release planning",
            body: "Can you check the launch date?",
            origin_app: "Notes",
            kind: "info",
            seen: false,
        ),
    )
    d.notices.insert(
        Notice(
            key: "n2",
            title: "Three threads filed",
            body: "Fieldwork filed 3 threads into Receipts.",
            origin_app: "Fieldwork",
            kind: "success",
            seen: false,
        ),
    )
    return d.notices.len()
}

/// What the centre says of them: how many are waiting, or that none is.
fn unread_line(d: Desk) -> Text {
    let waiting = unread(d)
    if waiting == 0 { "All read" } else if waiting == 1 { "1 unread" } else { "{waiting} unread" }
}

/// One sender's notices, under its name: each with its title, what it says and its actions.
view notice_group(d: Desk, sender: Text, notices: List<Notice>) {
    ui.Section(sender) {
        for n in notices {
            notice_row(d, n)
        }
    }
}

/// One notice: its title, what it says, its sender, its actions and the way to dismiss it. An
/// unread one is lifted and bears a dot in the person's colour, toned by its kind, so a failure
/// reads as one at a glance.
view notice_row(d: Desk, n: Notice) {
    ui.ListRow(n.title, secondary: n.body, meta: n.origin_app, selected: not n.seen) {
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
        for a in actions_of(d, n) {
            ui.Button(a.label, size: .small) {
                on press: device.act_on_notice(device.desk, notice: n.key, action: a.key)
            }
        }
        ui.Button("Dismiss", icon: .x, icon_only: true, size: .small, role: .plain) {
            on press: device.dismiss_notice(device.desk, notice: n.key)
        }
    }
}

/// A banner: the newest notice, risen over the desk, with its first action and a dismiss. The
/// session takes it away when it is dismissed or its sender closes it, and names the one it has
/// up (`Desk.banner`), so the banner and the centre never disagree about what just arrived.
/// What the person was told is held back while the desk is quiet.
///
/// A desk with room shows the toolkit's banner; a narrow one shows the notice as a row with its
/// actions under it, so nothing is cut at any text size.
view banner(d: Desk) {
    if let n = d.notices.find(n -> n.key == d.banner) {
        if not d.quiet {
            if space.compact or space.width < 44.0 {
                banner_row(d, n)
            } else {
                banner_full(d, n)
            }
        }
    }
}

/// A banner on a desk with the room: the notice with its kind's tone and its first action.
view banner_full(d: Desk, n: Notice) {
    let actions = actions_of(d, n)
    ui.Banner(
        n.title,
        message: n.body,
        kind: if n.kind == "error" {
            .error
        } else if n.kind == "warning" {
            .warning
        } else if n.kind == "success" { .success } else { .info },
        action: if not actions.is_empty() { actions.first()?.label ?? "Open" },
    ) {
        on act: device.act_on_notice(device.desk, notice: n.key, action: actions.first()?.key ?? "")
        on dismiss: device.dismiss_notice(device.desk, notice: n.key)
    }
}

/// A banner on a narrow desk: the notice as a row, its actions under it.
view banner_row(d: Desk, n: Notice) {
    stack(gap: .related) {
        ui.ListRow(n.title, secondary: n.body) {
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
            for a in actions_of(d, n) {
                ui.Button(a.label, size: .small) {
                    on press: device.act_on_notice(device.desk, notice: n.key, action: a.key)
                }
            }
            ui.Button("Dismiss", icon: .x, icon_only: true, size: .small, role: .plain) {
                on press: device.dismiss_notice(device.desk, notice: n.key)
            }
        }
    }
}

/// The actions `n` offers, in its sender's order.
fn actions_of(d: Desk, n: Notice) -> List<NoticeAction> {
    return d.notice_actions.filter(a -> a.notice == n.key)
}

/// What a list of notices was sent by, each once, in the order the notices came.
fn groups_of(notices: List<Notice>) -> List<Text> {
    return notices.map(n -> n.origin_app).group(g -> g).map(p -> p.key)
}
