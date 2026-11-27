/// The shell's own cards over the windows: the emoji picker, what this computer is, and the keys
/// a held Super reaches. Each is the session's to say (`device.overlays`) and this package's to
/// draw, so a person sees the same picker, sheet and key card on a shell that draws the screen as
/// on one that does not.
///
/// The emoji picker: the toolkit's one card, with its search, groups, tones and recents, opened by
/// its chord. It is the shell's view while the shell draws the screen, so it takes the keys the
/// person types into it, and each pick it reports is written into the field they were typing in,
/// exactly as the picker's button in an app writes one.
view emoji_card() {
    ui.EmojiPane(open: true) {
        on pick(e: Text): device.pick_emoji(device.overlays, text: e)
        on dismiss: device.close_emoji(device.overlays)
    }
}

/// What this computer is: its name, the Tilo it runs, where to read about it, and the settings
/// pages with every other detail. A sheet over the windows, as the chrome's `about` verb opens it;
/// Escape or Close takes it away.
view about_sheet() {
    if let a = device.overlays.about_sheet {
        about_card(a)
    }
}

/// The About sheet's content, as the session reads what this computer is.
view about_card(a: device.AboutSheet) {
    ui.ModalDialog(
        title: a.title,
        description: if a.heading != "" { a.heading } else { a.version },
        open: true,
        primary: "Updates",
        secondary: "Close",
    ) {
        on act_primary: session
            .open_settings(link: "settings://updates")
            .then(session.about(open: false))
        on act_secondary: session.about(open: false)
        on dismiss: session.about(open: false)
        stack(gap: .tight) {
            if a.heading != "" {
                ui.Text(a.heading, role: .subheading)
            }
            ui.Text(a.version, role: .caption, secondary: true)
            if a.home != "" {
                ui.Link(a.home)
            }
            ui.Button("All details", icon: .info) {
                on press: session
                    .open_settings(link: "settings://about")
                    .then(session.about(open: false))
            }
        }
    }
}

/// The keys a held Super reaches, at the foot of the display the person works on: one column per
/// group, the group's title over its chords and what each does. It is a picture: it takes no focus,
/// no key and no pointer, so every chord still runs while it shows, and letting Super go puts it
/// away.
view peek_card(p: device.Peek) {
    row(at: .bottom, fill, align: .center, distribute: .center, padding: .group) {
        ui.Card {
            row(gap: .section, align: .start, wrap: true) {
                for g in p.groups {
                    stack(gap: .tight) {
                        ui.Text(g.title, role: .caption, secondary: true)
                        ui.KeyHints {
                            for h in g.hints {
                                ui.KeyHint(h.keys, h.label)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The power menu: what this session may do to itself and this computer — locking, logging out,
/// switching user, sleeping, restarting or shutting down — as rows under the control that opened
/// it: the quick settings' Power button, else the bar's trailing end for one a key or an agent
/// opened. The session says what it offers and each row is its own action (`desktop:power-menu`),
/// so what a press does, the question a destructive one asks first and what an agent can ask for
/// are one thing.
view power_sheet() {
    let m = device.power_menu
    if m.open {
        row(fill, align: .start, distribute: .end) {
            ui.Popover(label: "Power", open: true, side: .below) {
                on dismiss: device.close(m)
                stack(gap: .tight) {
                    ui.Text("Power", role: .caption, secondary: true)
                    for e in m.offers {
                        if e == "lock" {
                            ui.Button("Lock", icon: .lock, role: .plain) {
                                on press: device.power_lock(m)
                            }
                        } else if e == "log-out" {
                            ui.Button("Log out", icon: .arrow_right, role: .plain) {
                                on press: device.power_log_out(m)
                            }
                        } else if e == "switch-user" {
                            ui.Button("Switch user", icon: .users, role: .plain) {
                                on press: device.power_switch_user(m)
                            }
                        } else if e == "suspend" {
                            ui.Button("Sleep", icon: .moon, role: .plain) {
                                on press: device.power_suspend(m)
                            }
                        } else if e == "restart" {
                            ui.Button("Restart", icon: .rotate_cw, role: .plain) {
                                on press: device.power_restart(m)
                            }
                        } else if e == "shut-down" {
                            ui.Button("Shut down", icon: .power, role: .plain) {
                                on press: device.power_shut_down(m)
                            }
                        }
                    }
                }
            }
        }
    }
}
