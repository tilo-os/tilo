/// The desk style's own settings, and the two ways of changing a desk (`docs/designs/app-language/
/// shell.md` §6): without an agent, in the one place a person's settings live, and with an agent,
/// by the same actions the person's controls run.
///
/// Every setting here is plain tilolang state with a default (`chrome.tl`'s `Shell` record), so
/// a person who never touches it gets the style as it ships, and the values live in the
/// arrangement's own record, synced with the person's devices, with history and undo.
///
/// Each setting is one action, so an agent — the canvas conversation or the REPL — changes it
/// with the same call a press does, attributed and undoable: `shell.set_dock_edge(shell.shell,
/// edge: shell.Edge.left)`. The style's settings stand over the desk (`style_panel`), opened by
/// the bar's style menu (`style_menu`, `strips.tl`): a person changes the desk they look at
/// without leaving it, the way `shell.md` §6's "Two ways to change a desk" asks.
///
/// Opens the chosen style's settings over the desk: what the bar's style menu's entry runs, and
/// what the REPL or the canvas agent calls too.
action open_style_settings(s: Shell) (icon: .settings, label: "Desk style settings") {
    s.settings_open = true
}

/// Closes them.
action close_style_settings(s: Shell) (icon: .x, label: "Close") {
    s.settings_open = false
}

/// Shows or hides their being open: one control for both, so a person sees the state where they
/// press it.
action show_style_settings(s: Shell, on: Bool = not s.settings_open) (
    icon: .settings,
    label: "Desk style settings",
    on: s.settings_open,
) {
    s.settings_open = on
}

/// Whether the strip of several places shows its tab bar: the strips style's own setting.
action set_strips_tabs(s: Shell, on: Bool) (label: "Strip tabs", group: "Strips") {
    s.strips_tabs = on
    s.tuned = true
}

/// Whether a place let go on the framed canvas lands on the grid: the frames style's own
/// setting.
action set_frames_snap(s: Shell, on: Bool) (label: "Snap places to a grid", group: "Frames") {
    s.frames_snap = on
    s.tuned = true
}

/// The chosen style's settings, in a panel over the desk: the settings the style the person
/// shows declares, and nothing else, so a style's own settings are its own view and the system
/// keeps no per-style list (`shell.md` §6: "Nothing in Settings is per style").
view style_panel(d: Desk) {
    let s = shell
    let chosen = style_of(viewing)
    row(fill, align: .center, distribute: .center, padding: .related) {
        ui.FloatingPanel(width: if not space.compact { 360.0 }) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("Settings for {style_title(viewing, d)}", role: .heading)
                    spacer
                    ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
                        on press: close_style_settings(s)
                    }
                }
            }
            body: {
                stack(gap: .section) {
                    if chosen == "so.tilo.shell:desktop" {
                        desktop_style_settings(s)
                    } else if chosen == "so.tilo.shell:strips" {
                        strips_style_settings(s)
                    } else if chosen == "so.tilo.shell:frames" {
                        frames_style_settings(s)
                    } else if chosen == "so.tilo.shell:spaces" {
                        spaces_style_settings(s)
                    } else if let declared = d.styles.find(x -> x.style == chosen) {
                        // A style another package declares: its own settings are its own
                        // record's, which the system cannot read (`shell.md` §6: "A style an
                        // agent writes gets settings the same way: by declaring actions on its
                        // own record"). Its description stands here, and Settings opens it.
                        ui.Text(
                            "{style_label(declared)} comes from {declared.package}, and its settings are its package's own.",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            }
        }
    }
}

/// The desktop style's settings: where the dock rides, how big it is, whether it hides itself,
/// and whether the top bar shows, each an action of `chrome.tl`'s own.
view desktop_style_settings(s: Shell) {
    form {
        toggle("Show the top bar", bind: s.bar_shown)
        toggle("Show the dock", bind: s.dock_shown)
        field("Dock edge") {
            for edge in cases(Edge) {
                button(edge.label(), on: dock_on(s, edge))
            }
        }
        field("Dock size") {
            for size in cases(Size) {
                button(size.label(), on: set_dock_size(s, size))
            }
        }
        field("Dock, when it shows") {
            for showing in cases(Showing) {
                button(showing.label(), on: set_dock_showing(s, showing))
            }
        }
    }
}

/// The strips style's settings: whether a strip of several places shows its tab bar.
view strips_style_settings(s: Shell) {
    form {
        toggle("Show the tabs of a strip", bind: s.strips_tabs)
    }
}

/// The frames style's settings: whether a place let go on the canvas lands on the grid.
view frames_style_settings(s: Shell) {
    form {
        toggle("Snap places to a grid", bind: s.frames_snap)
    }
}

/// The spaces style's settings. None of its own yet: it shows each place as its own view and
/// carries no chrome of its own, so it says so rather than showing another style's controls.
view spaces_style_settings(s: Shell) {
    ui.Text("Spaces shows each place as its own view and has nothing to set.", role: .caption)
}
