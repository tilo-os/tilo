/// Layouts: the presets a person picks among, each a named set of the settings `chrome.tl`
/// holds, and the individual settings underneath for anyone who wants to tune one thing.
/// Picking a preset applies it live through the shared motion primitives; changing one
/// setting after that makes the result "Custom, based on X" and never loses the preset.
///
/// Each preset's card draws the shell **with the real views**: the bar's parts and the dock's
/// items in miniature, on the edge and in the fit and material the preset names, so what a
/// person picks is what they get.
/// The layout picker: a card per preset, the one in use marked, and the sentence saying where
/// the settings stand.
view layouts(s: Shell) {
    section("Layout") {
        row(gap: .related, align: .center) {
            ui.Text(standing(s), role: .subheading)
            if s.tuned {
                spacer
                button(reset(s))
            }
        }
        grid(min: 15rem, gap: .section) {
            for preset in cases(Layout) {
                layout_card(s, preset)
            }
        }
    }
}

/// What the person's settings stand as: the preset's name, or "Custom, based on" it.
fn standing(s: Shell) -> Text {
    if s.tuned {
        if s.based_on == .tilo {
            "Custom, based on Tilo"
        } else if s.based_on == .macos {
            "Custom, based on macOS"
        } else if s.based_on == .windows {
            "Custom, based on Windows"
        } else if s.based_on == .gnome {
            "Custom, based on GNOME"
        } else if s.based_on == .windows_desktop {
            "Custom, based on the traditional desktop"
        } else { "Custom, based on Minimal" }
    } else if s.preset == .tilo {
        "Tilo"
    } else if s.preset == .macos {
        "Like macOS"
    } else if s.preset == .windows {
        "Like Windows"
    } else if s.preset == .gnome { "Like GNOME" } else { "Minimal" }
}

/// One preset's card: its name, what it is, and a miniature of the shell it makes, drawn
/// from the preset's own settings rather than pictured.
view layout_card(s: Shell, preset: Layout) {
    let chosen = s.preset == preset and not s.tuned
    ui.Card(title: preset.label(), selected: chosen) {
        body: {
            preview(preset)
            ui.Text(describe(preset), role: .caption, secondary: true)
            if chosen {
                ui.Badge("In use", tone: .success)
            } else {
                button(apply(s, preset))
            }
        }
    }
}

/// What a preset is, in a sentence.
fn describe(preset: Layout) -> Text {
    if preset == .tilo {
        "A dock at the bottom and the top bar, always shown."
    } else if preset == .macos {
        "A floating, rounded dock that hides when a window covers it; the bar carries the app's menus."
    } else if preset == .windows {
        "A full-width taskbar at the bottom, always shown."
    } else if preset == .gnome {
        "The top bar alone; the overview on demand."
    } else if preset == .windows_desktop {
        "A floating desk of windows, each with its own bar, the focused window's commands in the top bar, and a dock at the foot."
    } else {
        "No chrome at rest: the screen edges reveal the bar and the dock."
    }
}

/// A preset's shell in miniature: where its bar and dock stand, how they sit, and what they hold,
/// drawn with the same views the shell itself uses at the size a card gives them.
view preview(preset: Layout) {
    layers {
        row(fill, align: .center, padding: .tight) {
            if preset == .tilo
                or preset == .macos
                or preset == .gnome
                or preset == .minimal
                or preset == .windows_desktop {
                preview_bar(preset)
            }
        }
        row(at: .bottom, fill, align: .center, padding: .tight) {
            if preset == .tilo
                or preset == .macos
                or preset == .windows
                or preset == .windows_desktop {
                preview_dock(preset)
            }
        }
    }
}

/// The miniature bar: its parts, left to right, in the fit the preset gives it.
view preview_bar(preset: Layout) {
    row(gap: .tight, align: .center, fill) {
        ui.Icon(.tilo, size: .small)
        if preset == .macos or preset == .windows_desktop {
            ui.Text("File", role: .caption)
            ui.Text("Edit", role: .caption)
            ui.Text("View", role: .caption)
        }
        spacer
        ui.Text("4:28", role: .caption, numeric: true)
        spacer
        ui.Icon(.bell, size: .small)
    }
}

/// The miniature dock: the launcher and three items, on the fit the preset gives it.
view preview_dock(preset: Layout) {
    if preset == .macos or preset == .windows_desktop {
        row(gap: .tight, align: .center) {
            ui.Icon(.layout_grid, size: .small)
            ui.Icon(.file_text, size: .small)
            ui.Icon(.message_square, size: .small)
            ui.Icon(.mail, size: .small)
        }
    } else {
        row(gap: .tight, align: .center, fill) {
            ui.Icon(.layout_grid, size: .small)
            ui.Icon(.file_text, size: .small)
            ui.Icon(.message_square, size: .small)
            spacer
            ui.Icon(.mail, size: .small)
        }
    }
}

/// The settings underneath the presets: the bar's and the dock's, each change one action.
view shell_settings(s: Shell) {
    std.page(s) {
        stack(gap: .section) {
            layouts(s)
            section("Top bar") {
                form {
                    toggle("Show the top bar", bind: s.bar_shown)
                    ui.ToggleGroup(caption: "Edge", fill: true) {
                        for edge in cases(Edge) {
                            ui.Segment(edge.label(), chosen: s.bar_edge == edge) {
                                on press: bar_on(s, edge)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "Fit", fill: true) {
                        for fit in cases(Fit) {
                            ui.Segment(fit.label(), chosen: s.bar_fit == fit) {
                                on press: set_bar_fit(s, fit)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "Material", fill: true) {
                        for finish in cases(Finish) {
                            ui.Segment(finish.label(), chosen: s.bar_finish == finish) {
                                on press: set_bar_finish(s, finish)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "When it shows", fill: true) {
                        for showing in cases(Showing) {
                            ui.Segment(showing.label(), chosen: s.bar_showing == showing) {
                                on press: set_bar_showing(s, showing)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "Size", fill: true) {
                        for size in cases(Size) {
                            ui.Segment(size.label(), chosen: s.bar_size == size) {
                                on press: set_bar_size(s, size)
                            }
                        }
                    }
                }
                bar_parts(s)
            }
            section("Dock") {
                form {
                    toggle("Show the dock", bind: s.dock_shown)
                    ui.ToggleGroup(caption: "Edge", fill: true) {
                        for edge in cases(Edge) {
                            ui.Segment(edge.label(), chosen: s.dock_edge == edge) {
                                on press: dock_on(s, edge)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "Fit", fill: true) {
                        for fit in cases(Fit) {
                            ui.Segment(fit.label(), chosen: s.dock_fit == fit) {
                                on press: set_dock_fit(s, fit)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "Material", fill: true) {
                        for finish in cases(Finish) {
                            ui.Segment(finish.label(), chosen: s.dock_finish == finish) {
                                on press: set_dock_finish(s, finish)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "When it shows", fill: true) {
                        for showing in cases(Showing) {
                            ui.Segment(showing.label(), chosen: s.dock_showing == showing) {
                                on press: set_dock_showing(s, showing)
                            }
                        }
                    }
                    ui.ToggleGroup(caption: "Size", fill: true) {
                        for size in cases(Size) {
                            ui.Segment(size.label(), chosen: s.dock_size == size) {
                                on press: set_dock_size(s, size)
                            }
                        }
                    }
                }
                dock_settings(s)
            }
        }
    }
}

/// The bar's parts, in the person's order, each shown or hidden, and the two the bar can
/// gain.
view bar_parts(s: Shell) {
    section("Parts, in order") {
        ui.VirtualList {
            for part in s.bar_parts {
                ui.ListItem(item: part) {
                    part_row(s, part)
                }
            }
        }
        row(gap: .related) {
            button(show_app_menus(s))
        }
    }
}

/// One bar part in the settings: its glyph, its name, and whether it shows.
view part_row(s: Shell, part: BarPart) {
    ui.ListRow(part.title, icon: part.slot.icon(), selected: part.shown) {
        trailing: {
            ui.Button(if part.shown { "Shown" } else { "Hidden" }, toggled: part.shown) {
                on press: show_part(s, part, not part.shown)
            }
        }
    }
}

/// The dock's settings the form does not hold: what it shows besides the pinned items.
view dock_settings(s: Shell) {
    form {
        toggle("Running apps", bind: s.dock_running)
        toggle("Launcher entry", bind: s.dock_launcher)
        toggle("Magnify the item under the pointer", bind: s.dock_zoom)
    }
}
