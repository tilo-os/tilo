/// The keys a surface shows under it, as `ui.KeyHints`: the drawn keycaps of each chord and
/// what pressing them does, side by side, in the person's own keyboard legends. The shell never
/// writes a chord as a string, so a German keyboard's legends and a Mac's Command key read
/// right.
/// The keys of the launcher: typing searches, the arrows walk the rows, Tab and Shift+Tab walk
/// the groups, Enter opens, Esc closes.
view key_hints_launcher() {
    ui.KeyHints {
        ui.KeyHint(["Up", "Down"], "Choose")
        ui.KeyHint(["Tab"], "Next group")
        ui.KeyHint(["Enter"], "Open")
        ui.KeyHint(["Esc"], "Close")
    }
}

/// The keys of the switcher: hold the modifier, Tab walks, letting go goes there. The walk that
/// keeps to one app is the modifier and the grave key, so that is the one named while it does.
view key_hints_switcher(this_app: Text) {
    ui.KeyHints {
        ui.KeyHint(if this_app != "" { ["Alt", "`"] } else { ["Alt", "Tab"] }, "Walk")
        ui.KeyHint(["Enter"], "Go there")
        ui.KeyHint(["Esc"], "Stay")
    }
}

/// A window whose app stopped answering says so, with the two ways out: keep waiting for it, or
/// quit it. Wait gives the app another period to answer and takes the card away; Force quit ends
/// the app, and the window with it. Both are the window's own published actions, so a script or an
/// agent takes them the same way.
view hung_window(w: device.Window) {
    stack(gap: .tight) {
        ui.Badge("Not responding", tone: .warning)
        row(gap: .tight, align: .center, wrap: true) {
            ui.Button("Wait") {
                on press: session.wait_window(window(w))
            }
            ui.Button("Force quit", role: .destructive) {
                on press: session.force_quit_app(window(w))
            }
        }
    }
}

/// The card a window's own place shows while its app stopped answering: the same words as the
/// card in the overview and the switcher, standing over the window it is about so the person
/// answers it where they are working, not only where they look over their windows.
view hung_card(w: device.Window) {
    row(fill, align: .center, distribute: .center, padding: .group) {
        ui.Card {
            hung_window(w)
        }
    }
}

/// A window that runs on another device, while nothing of it stands here: which app's window it
/// is, the device it runs on, and that it opens there. The workspace's layout lists it on every
/// shell as its device's, so this shell says where it is rather than showing an empty well with
/// nothing to say why.
view window_elsewhere(w: device.Window) {
    ui.EmptyState(
        if w.device_name != "" {
            "{w.origin_app} on {w.device_name}"
        } else { "{w.origin_app} on another device" },
        message: "This window runs on that device. Open it there to see it.",
        icon: .monitor,
        inline: true,
    )
}
