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
                    // How many are waiting changes as notices arrive while the person is
                    // elsewhere: it is announced politely, so a reader hears it without having
                    // to come back to the centre.
                    ui.Text(unread_line(), role: .caption, secondary: true, live: .polite)
                    spacer
                    if not all.is_empty() {
                        ui.Button("Clear all", icon: .trash, size: .small) {
                            on press: device.clear(device.notifications)
                        }
                    }
                }
            }
            body: {
                if let why = device.notifications.quiet {
                    ui.Text(why, role: .caption, secondary: true)
                }
                if device.notifications.undo {
                    undo_band()
                }
                asking_list()
                if all.is_empty() {
                    if asked_count() == 0 {
                        ui.EmptyState("Nothing to catch up on", icon: .bell)
                    }
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
    ui.ListRow(
        n.title,
        secondary: n.body,
        meta: n.from,
        selected: not n.seen,
        value: said_ago(n.age_seconds),
    ) {
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
    asked_banners()
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
        message: if n.body != "" and n.preview { n.body },
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
            selected: n.number == (device.banners.focused ?? 0),
        ) {
            trailing: {
                ui.Badge(
                    dot: true,
                    tone: if n.kind == "error" {
                        .danger
                    } else if n.kind == "warning" { .warning } else { .info },
                )
            }
            on activate: device.open_banner(device.banners, banner: "{n.number}")
        }
        row(gap: .related, wrap: true) {
            // A call's answer and decline are their own actions, as a snooze is: each press is
            // the banner resource's own, and the rest of the sender's choices run as a choice.
            for a in n.actions {
                if a.key == "answer" or a.key == "accept" {
                    ui.Button("Answer", role: .primary, size: .small) {
                        on press: device.answer_call(device.banners, banner: "{n.number}")
                    }
                } else if a.key == "decline" or a.key == "reject" or a.key == "hangup" {
                    ui.Button("Decline", size: .small, role: .plain) {
                        on press: device.decline_call(device.banners, banner: "{n.number}")
                    }
                } else if a.key == "snooze" {
                    ui.Button("Snooze", size: .small, role: .plain) {
                        on press: device.snooze(device.banners, banner: "{n.number}")
                    }
                } else if a.key != "inline-reply" {
                    ui.Button(a.label, size: .small) {
                        on press: device.act(
                            device.notifications,
                            notification: n.number,
                            action: a.key,
                        )
                    }
                }
            }
            ui.Button("Read", size: .small, role: .plain) {
                on press: device.read(device.banners, banner: "{n.number}")
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

/// How long ago a notice arrived, in words: a person reads a moment or a while, not a clock.
fn said_ago(seconds: Int) -> Text {
    if seconds < 60 {
        "Just now"
    } else if seconds < 3600 {
        "{seconds / 60} min ago"
    } else if seconds < 86400 { "{seconds / 3600} h ago" } else { "{seconds / 86400} d ago" }
}
