/// The notifications: the banners that arrive, and the centre that keeps them. Both are views of
/// the centre the session publishes (`device.notifications`), grouped by what sent them: an app, or
/// the agent task that did. Each notice carries its sender's own actions, in the order its sender
/// put them (`device.act`), and a notice that offers a reply takes the person's words back to its
/// sender (`device.reply`).
///
/// Banners are a stack, newest first, as many as fit; what does not fit is one row that opens the
/// centre, where every one waits. The session takes a banner away a few seconds after it arrived,
/// so the stack empties by itself.
///
/// The notification centre is a floating panel at the screen's trailing edge, over what it covers,
/// on the theme's material as every notification centre is: its header says how many are waiting
/// and holds the way to clear them all, an Undo band brings them back after a Clear all, and each
/// sender's group opens as one on the toolkit's disclosure motion.
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
                if device.notifications.undo {
                    undo_band()
                }
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

/// Bring back what a Clear all took, while the session still holds it.
view undo_band() {
    row(gap: .related, align: .center, wrap: true) {
        ui.Text("Notifications cleared", role: .caption, secondary: true)
        spacer
        ui.Button("Undo", role: .secondary, size: .small) {
            on press: device.undo_clear(device.notifications)
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

/// One notice: its title, what it says, its sender, its actions, the way to hand it to the
/// person's agent and the way to dismiss it. An unread one is lifted and bears a dot in the
/// person's colour, toned by its kind, so a failure reads as one at a glance. Pressing it opens
/// what it names.
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
        on activate: device.open_notice(device.notifications, notification: n.number)
    }
    row(gap: .related, align: .center, wrap: true) {
        for a in n.actions {
            if a.key != "inline-reply" {
                ui.Button(a.label, size: .small) {
                    on press: device.act(
                        device.notifications,
                        notification: n.number,
                        action: a.key,
                    )
                }
            }
        }
        ui.Button("Hand to agent", icon: .sparkles, size: .small, role: .plain) {
            on press: device.hand_to_agent(device.notifications, notification: n.number)
        }
        ui.Button("Dismiss", icon: .x, icon_only: true, size: .small, role: .plain) {
            on press: device.dismiss(device.notifications, notification: n.number)
        }
    }
}

/// The banners standing now, newest first, as many as fit, and a row that opens the centre when
/// more wait behind them than show. What the person was told is held back while notifications are
/// silenced.
view banner() {
    let centre = device.notifications
    if (centre.quiet ?? "") == "" {
        let ids = standing_banners(centre)
        if not ids.is_empty() {
            let shown = ids.take(3)
            let more = ids.len() - shown.len()
            stack(gap: .related) {
                for id in shown {
                    if let n = centre.notices.find(n -> n.number == id) {
                        if space.compact or space.width < 44.0 {
                            banner_row(n)
                        } else {
                            banner_full(n)
                        }
                    }
                }
                if more > 0 {
                    ui.Button(
                        if more == 1 {
                            "1 more notification"
                        } else { "{more} more notifications" },
                        icon: .bell,
                        size: .small,
                        role: .plain,
                    ) {
                        on press: device.show_surface(device.overlays, surface: "centre")
                    }
                }
            }
        }
    }
}

/// The ids of the banners standing now: the stack the session publishes, else the one banner an
/// older session names.
fn standing_banners(centre: device.Notifications) -> List<Int> {
    if not centre.banners.is_empty() {
        return centre.banners
    }
    if let one = centre.banner {
        return [one]
    }
    return []
}

/// A banner on a wide screen: the notice with its kind's tone, what sent it, its actions and its
/// reply. Each part it has is the part it shows: a notice that says nothing puts no line under the
/// title, and one with no sender names none above it.
view banner_full(n: device.Notice) {
    let actions = n.actions
    let can_reply = actions.any(a -> a.key == "inline-reply")
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
    if can_reply {
        reply_row(n)
    }
}

/// A banner on a narrow screen: the notice as a row with what sent it, its actions under it.
view banner_row(n: device.Notice) {
    let can_reply = n.actions.any(a -> a.key == "inline-reply")
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
            on activate: device.open_notice(device.notifications, notification: n.number)
        }
        row(gap: .related, wrap: true) {
            for a in n.actions {
                if a.key != "inline-reply" {
                    ui.Button(a.label, size: .small) {
                        on press: device.act(
                            device.notifications,
                            notification: n.number,
                            action: a.key,
                        )
                    }
                }
            }
            ui.Button("Dismiss", icon: .x, icon_only: true, size: .small, role: .plain) {
                on press: device.dismiss(device.notifications, notification: n.number)
            }
        }
        if can_reply {
            reply_row(n)
        }
    }
}

/// The way to answer a banner that offers a reply: what the person types goes back to its sender,
/// and the banner takes it there and goes.
view reply_row(n: device.Notice) {
    state draft: Text = ""
    row(gap: .related, align: .center, wrap: true) {
        ui.TextInput(bind: draft, placeholder: "Reply", fill)
        ui.Button(
            "Send",
            role: .primary,
            size: .small,
            enabled: draft != "",
            description: "Write a reply first",
        ) {
            on press: device.reply(device.notifications, notification: n.number, text: draft)
        }
    }
}

/// What a list of notices was sent by, each once, in the order the notices came.
fn groups_of(notices: List<device.Notice>) -> List<Text> {
    return notices.map(n -> n.from).group(g -> g).map(p -> p.key)
}
