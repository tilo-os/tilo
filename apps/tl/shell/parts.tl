/// The keys a surface shows under it, as `ui.KeyHints`: the drawn keycaps of each chord and
/// what pressing them does, side by side, in the person's own keyboard legends. The shell never
/// writes a chord as a string, so a German keyboard's legends and a Mac's Command key read
/// right.
/// The keys of the launcher: typing searches, the arrows walk the rows, Tab and Shift+Tab walk
/// the groups, Enter opens, a modifier with it opens the row another way, Esc closes.
view key_hints_launcher() {
    ui.KeyHints {
        ui.KeyHint(["Up", "Down"], "Choose")
        ui.KeyHint(["Tab"], "Next group")
        ui.KeyHint(["Enter"], "Open")
        ui.KeyHint(["Shift", "Enter"], "Open as a tab")
        ui.KeyHint(["Ctrl", "Enter"], "Open beside")
        ui.KeyHint(["Alt", "Enter"], "More actions")
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

/// The window's own shape, as the compositor draws it: every place that pictures a window takes
/// this aspect, so the picture fills the place and no ground shows round it. A window with no
/// window here to measure (one on another device, or a placeholder what its app left) takes the
/// shell's own shape for a window.
fn window_aspect(w: device.Window) -> Float {
    if w.width <= 0 or w.height <= 0 { 1.6 } else { w.width.to_float() / w.height.to_float() }
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

/// The card a window shows where its app left: what happened, why, what the app itself said of it,
/// quoted, and the two answers — ask for the app again in this same place, or dismiss what it says.
/// Everything the launch printed waits behind Details. Reopen keeps the tab, so the window that
/// comes back lands where this one stood.
view crashed_card(w: device.Window) {
    if let e = w.ended {
        row(fill, align: .center, distribute: .center, padding: .group) {
            ui.Card {
                stack(gap: .tight) {
                    ui.Badge("App ended", tone: .warning)
                    ui.Text(e.what, role: .title)
                    if e.why != "" {
                        ui.Text(e.why, secondary: true)
                    }
                    if e.said != "" {
                        ui.Quote(e.said)
                    }
                    if e.details != "" {
                        ui.DisclosureRow("Details") {
                            ui.Text(e.details, role: .caption, secondary: true)
                        }
                    }
                    row(gap: .tight, align: .center, wrap: true) {
                        if let label = e.retry {
                            ui.Button(label) {
                                on press: session.reopen_closed(window: w.key)
                            }
                        } else {
                            ui.Button("Reopen") {
                                on press: session.reopen_closed(window: w.key)
                            }
                        }
                        ui.Button("Dismiss", role: .plain) {
                            on press: session.close_tab(window(w))
                        }
                    }
                }
            }
        }
    }
}

/// A window that runs on another device, while nothing of it stands here: which app's window it/// A window that runs on another device, while nothing of it stands here: which app's window it
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

/// A window whose place just left the layout, drawn where the style showed it: its frame stands
/// at the last spot the layout kept for it and plays its departure, back into the control the
/// person opened it from. It takes nothing while it goes — no press, no drag, no drop — so the
/// actions of a place that has left the layout are never reachable. Every style draws a departing
/// window through this one view.
view leaving_place(s: LayoutStrip, p: LayoutPlace) {
    ui.WindowFrame(name: place_title(p), leaving: true, control: s.control, fill) {
        body: {
            // The place's own view, still, while the frame goes: the window is whole as it leaves.
            place_content(p)
        }
    }
}
