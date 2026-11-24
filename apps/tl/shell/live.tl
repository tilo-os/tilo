/// The bar's pill: the thing that went live last, and how many more there are. A press opens
/// quick settings, where every one of them stands.
// What is live now, as the session publishes it (`device.live`): a video playing, a timer
// running, a call, on this device or on another of the workspace's. A row of a resource is drawn
// by that resource's own `view live`, whose buttons are its type's own actions, so a pause here
// is the pause the app's window, a media key, the lock screen and an agent use. A row of what
// this machine plays or runs that no resource holds (a player heard over MPRIS, an app's
// activity) is drawn here from what the session reads of it, and its controls go back through
// the session.
view live_pill() {
    // What the person kept off this bar stays in quick settings.
    let rows = device.live.rows.filter(r -> not r.collapsed)
    // Only a wide bar has room for what is live itself, its name and its play button.
    let roomy = space.width_class == .wide
    if let newest = rows.last() {
        if space.compact {
            // A phone's bar has room for one control: it pulls what went live last out as a
            // card, dropped from under the bar, and folds it back.
            ui.Button(
                if newest.expanded { "Fold {live_name(newest)} back" } else { "{live_name(newest)}, live" },
                icon: if newest.expanded { .minimize_2 } else { .play },
                icon_only: true,
                role: .bar,
            ) {
                on press: if newest.expanded {
                    device.fold_live(device.live, key: newest.key)
                } else {
                    device.expand_live(device.live, key: newest.key)
                }
            }
        } else if not roomy {
            // A bar narrower than a wide one has no room for the thing itself: one control
            // says something is live, and opens quick settings, where it stands with its controls.
            ui.Button("{live_name(newest)}, live", icon: .play, icon_only: true, role: .bar) {
                on press: device.show_surface(device.overlays, surface: "quick-settings")
            }
        } else {
            row(gap: .related, align: .center) {
                live_row(newest, compact: true)
                if not newest.card {
                    // Pulled out of the pill, it stands as a card over the windows.
                    ui.Button("Show as a card", icon: .maximize_2, icon_only: true, role: .bar) {
                        on press: device.expand_live(device.live, key: newest.key)
                    }
                }
                if rows.len() > 1 {
                    ui.Button(
                        "{rows.len() - 1} more live",
                        icon: .chevron_down,
                        icon_only: true,
                        role: .bar,
                    ) {
                        on press: device.show_surface(device.overlays, surface: "quick-settings")
                    }
                }
            }
        }
    }
}

/// What a live row is called: its title, else the app or service it comes from.
fn live_name(r: device.LiveItem) -> Text {
    if r.title != "" { r.title } else { r.source }
}

/// Quick settings' section of what is live, the one live longest first: nothing while nothing is.
view live_section() {
    let rows = device.live.rows
    if not rows.is_empty() {
        stack(gap: .related) {
            ui.Text("Live now", role: .label, secondary: true)
            for r in rows {
                ui.Card {
                    stack(gap: .tight) {
                        live_row(r, compact: false)
                        live_card_controls(r)
                    }
                }
            }
        }
    }
}

/// One live thing: its resource's own view, with the device it plays on when that is another
/// one; otherwise what the session reads of it, with its controls.
view live_row(r: device.LiveItem, compact: Bool) {
    stack(gap: .tight) {
        if r.resource != "" {
            if let shown = resource(r.resource) {
                show(shown, view: "live")
            } else {
                ui.Text(live_name(r), role: .label, lines: 1, truncate)
            }
        } else {
            native_row(r, compact)
        }
        if not r.here and r.device != "" and not compact {
            ui.Text("On {r.device}", role: .caption, secondary: true, lines: 1, truncate)
        }
    }
}

/// A live card's own controls in quick settings: whether the bar shows it, and, for what an app
/// here runs that offers no end of its own, putting it away.
view live_card_controls(r: device.LiveItem) {
    row(gap: .tight, align: .center) {
        ui.Button(
            if r.collapsed { "Show on the bar" } else { "Keep off the bar" },
            icon: if r.collapsed { .eye } else { .minus },
            icon_only: true,
            role: .plain,
        ) {
            on press: device.keep_off_bar(device.live, key: r.key, off: not r.collapsed)
        }
        if not space.compact {
            ui.Button(
                if r.card { "Back to the bar" } else { "Show as a card" },
                icon: if r.card { .minimize_2 } else { .maximize_2 },
                icon_only: true,
                role: .plain,
            ) {
                on press: if r.card {
                    device.fold_live(device.live, key: r.key)
                } else {
                    device.expand_live(device.live, key: r.key)
                }
            }
        }
        if r.resource == "" and r.key != "player" and not r.controls.any(c -> c.ends) {
            ui.Button("Put away", icon: .x, icon_only: true, role: .plain) {
                on press: device.end_live(device.live, key: r.key)
            }
        }
    }
}

/// What this machine plays or runs that no resource holds: its name, what it reads now, and the
/// controls its app offers, each pressed back through the session. On the bar it is the name and
/// the play button; in quick settings the controls stand on a line of their own under the name.
view native_row(r: device.LiveItem, compact: Bool) {
    if compact {
        row(gap: .related, align: .center) {
            stack(fill) {
                ui.Text(live_name(r), role: .label, lines: 1, truncate)
            }
            for c in r.controls {
                if c.id == "play-pause" {
                    live_control(r, c, on_bar: true)
                }
            }
        }
    } else {
        stack(gap: .tight) {
            ui.Text(live_name(r), role: .label, lines: 1, truncate)
            if r.subtitle != "" {
                ui.Text(r.subtitle, role: .caption, secondary: true, lines: 1, truncate)
            } else if r.value != "" {
                ui.Text(r.value, role: .caption, secondary: true, numeric: true, lines: 1)
            }
            if not r.controls.is_empty() {
                row(gap: .tight, align: .center) {
                    for c in r.controls {
                        live_control(r, c, on_bar: false)
                    }
                }
            }
        }
    }
}

/// One control of a row the shell draws itself: a transport control as its glyph, any other by
/// its app's words.
view live_control(r: device.LiveItem, c: device.LiveControl, on_bar: Bool) {
    if c.id == "play-pause" or c.id == "previous" or c.id == "next" {
        ui.Button(
            c.label,
            icon: if c.id == "previous" {
                .skip_back
            } else if c.id == "next" {
                .skip_forward
            } else if r.playing { .pause } else { .play },
            icon_only: true,
            role: if on_bar { .bar } else { .plain },
        ) {
            on press: device.press_live(device.live, key: r.key, control: c.id)
        }
    } else {
        ui.Button(c.label, role: if on_bar { .bar } else { .plain }) {
            on press: device.press_live(device.live, key: r.key, control: c.id)
        }
    }
}

/// Whether the bar shows something live now: a narrow bar then gives the room it takes.
fn live_on_bar() -> Bool {
    not device.live.rows.filter(r -> not r.collapsed).is_empty()
}

/// The cards over the windows: what is live and stands as a card (`r.card`, which its app asks for
/// or the person pulls out of the pill), docked in a column in the corner of the display this
/// device's person chose (`shell.cards_corner`, the person's own), the one touched last nearest the corner. A
/// phone-width display has room for one card only, the one the person pulled out of the pill,
/// dropped full width from under the bar. The column changes at the shared pace, so a card that
/// comes slides in and one that goes slides out. A card stands on the display in use only.
view live_cards() {
    let cards = device.live.rows.filter(r -> r.card)
    // They stand on the display the person works on, as the bar's pill does in full; another
    // display's compact bar keeps to a glance.
    if not cards.is_empty() and on_primary() {
        if space.compact {
            if let r = cards.find(r -> r.expanded) {
                stack(at: .top, fill, padding: .group, motion: .standard) {
                    live_card(r)
                }
            }
        } else {
            // One room, the band under the bar, holds every card: a docked card stands in the
            // column at the chosen corner, and a card the person moved stands where they left
            // it, in the same room it is dragged in.
            let corner = shell.cards_corner
            let top = corner == .top_leading or corner == .top_trailing
            stack(
                fill,
                padding: .group,
                gap: .related,
                align: if corner == .top_leading or corner == .bottom_leading {
                    .start
                } else { .end },
                motion: .standard,
            ) {
                if not top {
                    spacer()
                }
                for r in cards.sort_by(r -> if top { r.place } else { 0 - r.place }) {
                    live_card(r)
                }
                if top {
                    spacer()
                }
            }
        }
    }
}

/// One live thing as a card over the windows: its app's glyph and name, a way back into the bar's
/// pill and its end; then what it is — its resource's own `view live`, or what the session reads of
/// it, where it is along its length and its controls.
view live_card(r: device.LiveItem) {
    ui.FloatingPanel(
        width: if not space.compact { 320.0 },
        fill: space.compact,
        label: "{live_name(r)} card",
        x: if not space.compact { r.x?.to_float() },
        y: if not space.compact { r.y?.to_float() },
    ) {
        on moved(at): device.move_live(device.live, r.key, at)
        header: {
            row(gap: .related, align: .center) {
                ui.Icon(
                    if r.kind == "call" {
                        .phone
                    } else if r.kind == "navigation" {
                        .navigation
                    } else if r.kind == "timer" {
                        .timer
                    } else if r.kind == "stopwatch" {
                        .clock
                    } else if r.kind == "music" or r.resource != "" { .music } else { .layers },
                    size: .small,
                )
                stack(fill) {
                    ui.Text(
                        if r.here or r.device == "" { r.source } else { "{r.source} on {r.device}" },
                        role: .caption,
                        secondary: true,
                        lines: 1,
                        truncate,
                    )
                }
                if not space.compact {
                    // Where this device's cards stand: a corner of each display, or back in it
                    // for a card the person moved.
                    ui.MenuButton("Card place", icon: .more_horizontal, icon_only: true, role: .plain) {
                        if r.x != .none {
                            ui.MenuItem("Back in the corner") {
                                on choose: device.move_live(device.live, key: r.key)
                            }
                            ui.MenuSeparator()
                        }
                        for c in cases(CardCorner) {
                            ui.MenuItem("Cards at the {c.label().lower()}", checked: shell.cards_corner == c) {
                                on choose: dock_cards(shell, c)
                            }
                        }
                    }
                }
                ui.Button("Back to the bar", icon: .minimize_2, icon_only: true, role: .plain) {
                    on press: device.fold_live(device.live, key: r.key)
                }
                if r.resource == "" and r.key != "player" {
                    ui.Button(
                        r.controls.find(c -> c.ends)?.label ?? "Put away",
                        icon: .x,
                        icon_only: true,
                        role: .plain,
                    ) {
                        on press: device.end_live(device.live, key: r.key)
                    }
                }
            }
        }
        body: {
            if r.resource != "" {
                if let shown = resource(r.resource) {
                    show(shown, view: "live")
                } else {
                    ui.Text(live_name(r), role: .heading, lines: 1, truncate)
                }
            } else if r.drawn {
                // Its app draws this card itself (a route, a call's video): the card holds the
                // block the app's own window stands over, and the session tells the app where.
                ui.Labelled("{live_name(r)} in {r.source}") {
                    ui.Skeleton(.block, height: r.drawn_height)
                }
            } else {
                card_body(r)
            }
        }
    }
}

/// What the session reads of something live no resource holds, as its card shows it: what it is
/// and what it reads, where it is along its length (a bar the person drags to go there), and its
/// controls — a transport as its glyphs, anything else in its app's words.
view card_body(r: device.LiveItem) {
    stack(gap: .related) {
        stack(gap: .tight) {
            ui.Text(live_name(r), role: .heading, lines: 1, truncate)
            if r.subtitle != "" {
                ui.Text(r.subtitle, role: .caption, secondary: true, lines: 1, truncate)
            }
            if r.value != "" and r.elapsed == "" {
                ui.Text(r.value, role: .title, numeric: true, lines: 1)
            }
        }
        if let along = r.progress {
            if r.elapsed != "" {
                row(gap: .related, align: .center) {
                    ui.Text(r.elapsed, role: .caption, secondary: true, numeric: true)
                    ui.Slider(value: along, label: "Position in {live_name(r)}", fill) {
                        on change(v): device.seek_live(device.live, key: r.key, fraction: v)
                    }
                    ui.Text(r.remaining, role: .caption, secondary: true, numeric: true)
                }
            } else {
                ui.ProgressBar(along, label: live_name(r))
            }
        }
        let offered = r.controls.filter(c -> not c.ends)
        if not offered.is_empty() {
            row(gap: .related, align: .center, distribute: .center) {
                for c in offered {
                    live_control(r, c, on_bar: false)
                }
            }
        }
    }
}
