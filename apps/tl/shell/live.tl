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
        if not roomy {
            // A bar narrower than a wide one has no room for the thing itself: one control
            // says something is live, and opens quick settings, where it stands with its controls.
            ui.Button("{live_name(newest)}, live", icon: .play, icon_only: true, role: .bar) {
                on press: device.show_surface(device.overlays, surface: "quick-settings")
            }
        } else {
            row(gap: .related, align: .center) {
                live_row(newest, compact: true)
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
        if r.resource == "" and r.key != "player" and not r.controls.any(c -> c.id == "close") {
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
