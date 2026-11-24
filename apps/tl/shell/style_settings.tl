/// The style's own settings, and the two ways of changing a style (`docs/designs/app-language/
/// shell.md` §6): without an agent, in the one place a person's settings live, and with an agent,
/// by the same actions the person's controls run.
///
/// Every setting here is plain tilolang state with a default (`chrome.tl`'s `Shell` document), so
/// a person who never touches it gets the style as it ships, and the values live in the
/// layout's own document, synced with the person's devices, with history and undo.
///
/// Each setting is one action, so an agent — the canvas conversation or the REPL — changes it
/// with the same call a press does, attributed and undoable: `shell.set_dock_edge(shell.shell,
/// edge: shell.Edge.left)`. The style's settings stand over the shell (`style_panel`), opened by
/// the bar's style menu (`style_menu`, `strips.tl`): a person changes the shell they look at
/// without leaving it, as `shell.md` §6 asks.
///
/// Opens the chosen style's settings over the shell: what the bar's style menu's entry runs, and
/// what the REPL or the canvas agent calls too.
// The styles themselves stand at the top of a style's settings panel (`style_settings_body`), so
// the canvas — whose bar keeps no switch of its own (`canvas.md` §7) — is one press from every
// other style, back to the strips included. (A plain comment on purpose: a `///` doc line is part
// of the app's published surface, which the SDK types and the app catalogue are generated from.)
action open_style_settings(s: Shell) (icon: .settings, label: "Style settings") {
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
    label: "Style settings",
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

/// The chosen style's settings, in a panel over the shell: the settings the style the person
/// shows declares, and nothing else, so a style's own settings are its own view and the system
/// keeps no per-style list (`shell.md` §6: "Nothing in Settings is per style").
view style_panel() {
    // The styles stand above the chosen style's settings: that choice belongs to the shell, not
    // to one style, so a person on the canvas — whose bar keeps no switch of its own — has it
    // here.
    let s = shell
    // A panel over the shell, set in from its edges as a surface does: what it sets down
    // never sits on the window's own text under it, and a phone's shell keeps a section's gutter.
    row(
        fill,
        align: .center,
        distribute: .center,
        padding: if space.compact { .section } else { .related },
    ) {
        ui.FloatingPanel(width: if not space.compact { 360.0 }) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("Settings for {style_title(viewing)}", role: .heading)
                    spacer
                    ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
                        on press: close_style_settings(s)
                    }
                }
            }
            body: {
                // The settings are as tall as what the style sets, and scroll only where a screen
                // is shorter than that: a panel over the shell never stands past it, and never
                // takes room it has nothing to put in.
                scroll {
                    style_settings_body()
                }
            }
        }
    }
}

/// The chosen style's settings, taking a phone's shell: a phone's shell has no room beside a panel, so
/// the settings take the screen and close back to it.
view style_sheet() {
    let s = shell
    stack(fill, padding: .section) {
        row(gap: .related, align: .center, wrap: true) {
            ui.Text("Settings for {style_title(viewing)}", role: .heading)
            spacer
            ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
                on press: close_style_settings(s)
            }
        }
        scroll(fill) {
            style_settings_body()
        }
    }
}

/// What a style sets, in the order its own view writes it: the style it shows now, and nothing
/// of another style's.
view style_settings_body() {
    // The style itself stands above them: the shell's choice, not one style's settings, so
    // every style's panel offers it — a person on the canvas, whose bar keeps no switch of its
    // own (`canvas.md` §7), moves to another style and back without leaving the settings they
    // opened.
    let s = shell
    let chosen = style_of(viewing)
    stack(gap: .related) {
        // The same switch the bar's menu and the viewing's own settings page offer, by the same
        // `show_style`: one action, whichever of them a person presses.
        style_switch(viewing)
        if chosen == "so.tilo.shell:desktop" {
            desktop_style_settings(s)
        } else if chosen == "so.tilo.shell:strips" {
            strips_style_settings(s)
        } else if chosen == "so.tilo.shell:frames" {
            frames_style_settings(s)
        } else if chosen == "so.tilo.shell:frameless" {
            frameless_style_settings(s)
        } else if let declared = styles.installed.offered.find(x -> x.style == chosen) {
            // A style another package declares: its own settings are its own document's, which the
            // system cannot read (`shell.md` §6: "A style an agent writes gets settings the same
            // way: by declaring actions on its own document"). Its description stands here, and
            // Settings opens it.
            ui.Text(
                "{style_label(declared)} comes from {declared.package}, and its settings are its package's own.",
                role: .caption,
                secondary: true,
            )
        }
        theme_settings()
    }
}

/// The look every style is drawn in: the person's appearance resource, the same fields Settings'
/// Appearance and Themes pages bind, so a change here is that one change, live in every window.
/// The shell keeps no copy in its layout document.
view theme_settings() {
    let a = settings.appearance
    stack(gap: .related) {
        ui.ToggleGroup(caption: "Look", fill: true) {
            for look in ["Tilo", "Paper", "Moss", "Dusk", "Mono"] {
                ui.Segment(look) {
                    on press: settings.choose_theme(a, theme: look)
                }
            }
        }
        for t in a.themes {
            ui.ListRow(t.name, secondary: if t.dark { "Dark" } else { "Light" }, icon: .palette) {
                on activate: settings.use_theme(a, theme: t.id)
            }
        }
        ui.Slider(
            bind: a.translucency,
            label: "Translucency",
            min: 0.0,
            max: 1.0,
            step: 0.05,
            shows_value: true,
        )
        ui.TextInput(bind: a.accent, placeholder: "Accent: follow the theme")
        let shown = if a.mode == "light" { a.light_theme } else { a.dark_theme }
        for t in a.themes {
            if "custom:" + t.id == shown {
                ui.Slider(
                    bind: t.window_radius,
                    label: "Window corners",
                    min: 0.0,
                    max: 28.0,
                    step: 1.0,
                    shows_value: true,
                )
                ui.Slider(
                    bind: t.refraction,
                    label: "Refraction",
                    min: 0.0,
                    max: 32.0,
                    step: 0.5,
                    shows_value: true,
                )
            }
        }
    }
}

/// The desktop style's settings: whether the top bar shows, where the dock rides, how big it is,
/// and when it shows. Each is one setting: its words, and the switch or the strip of choices that
/// sets it, so what a person changes stands beside what it changes.
view desktop_style_settings(s: Shell) {
    stack(gap: .related) {
        // A toggle carries its setting's name beside its switch and the whole row turns it, so the
        // row is the toggle alone: a text of the name beside it would say the setting twice, and
        // the two would meet at the panel's width.
        toggle("Show the top bar", bind: s.bar_shown)
        toggle("Show the dock", bind: s.dock_shown)
        // Each choice is one strip of the setting's cases, the case it holds filled, and a press
        // runs that setting's own action at the case pressed: one press here and an agent's call
        // change the dock the same way.
        ui.ToggleGroup(caption: "Dock edge", fill: true) {
            for edge in cases(Edge) {
                ui.Segment(edge.label(), chosen: s.dock_edge == edge) {
                    on press: dock_on(s, edge)
                }
            }
        }
        ui.ToggleGroup(caption: "Dock size", fill: true) {
            for size in cases(Size) {
                ui.Segment(size.label(), chosen: s.dock_size == size) {
                    on press: set_dock_size(s, size)
                }
            }
        }
        ui.ToggleGroup(caption: "When the dock shows", fill: true) {
            for showing in cases(Showing) {
                ui.Segment(showing.label(), chosen: s.dock_showing == showing) {
                    on press: set_dock_showing(s, showing)
                }
            }
        }
    }
}

/// The strips style's settings: whether a strip of several places shows its tab bar.
view strips_style_settings(s: Shell) {
    toggle("Show the tabs of a strip", bind: s.strips_tabs)
}

/// The frames style's settings: whether a place let go on the canvas lands on the grid.
view frames_style_settings(s: Shell) {
    toggle("Snap places to a grid", bind: s.frames_snap)
}

/// The frameless style's settings. None of its own yet: it shows each place as its own view and
/// carries no chrome of its own, so it says so rather than showing another style's controls.
view frameless_style_settings(s: Shell) {
    ui.Text("Frameless shows each place as its own view and has nothing to set.", role: .caption)
}
