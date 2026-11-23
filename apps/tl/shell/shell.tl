/// The shell: what this device shows. The person's bar across the top edge its settings name,
/// the dock on its own edge, and one surface over the windows at a time: the overview, the
/// switcher, search and launching, the notifications and the workspaces. Which
/// one is up is the session's (`device.overlays.here`), so the shell re-runs when it changes and nothing else
/// moves while the shell is at rest.
///
/// The bar and the dock are `ui.Chrome` bodies made of the theme's material for the style the
/// person picked (`bar_finish`, `dock_finish`): the theme's glass, let through and blurred over
/// the windows, or that same glass made calm, opaque with every light off — so a theme's material
/// and not the view decides what either is made of. Edge to edge and square at the `full` fit,
/// an inset rounded body at the floating one. Every surface is drawn
/// over them, so the bar stays reachable and a press outside a surface closes it, as the
/// session's chrome decides. Nothing here draws a window: the windows are the
/// compositor's, behind this view.
///
/// The shell, with the layout `front` filling the room between its bar and its foot, as the
/// style the person chose shows it (`screen`): the dock stands over that room on its own edge,
/// as the chrome's does, so a window takes the height the shell gives it rather than stopping
/// short of the dock. On a compact output that room is a phone's, too little for a window's bar to lose to a
/// floating dock, so the dock takes its own room at the foot instead and the window fills what
/// is left.
///
/// The switcher is a panel over the windows rather than a page of them: it takes that room and
/// centres itself there, where the surfaces that go on past a display are given the same room
/// as a page that scrolls. Until the session says its walk outlasted a flick
/// (`switcher_revealed`) the shell is the one at rest, so a flick between two windows shows
/// nothing over either.
///
/// `traditional` makes this the traditional desktop's chrome: its bar lists the window in
/// front's actions, and its own bar is drawn (`top_bar`). The desktop style passes it.
template view shell_around(front: Option<Layout>, traditional: Bool, content: View) {
    let s = shell
    let dock_takes_room = front != .none
        and dock_shows(s)
        and s.dock_edge == .bottom
        and space.compact
    let resting = device.overlays.here == ""
        or device.overlays.here == "none"
        or device.overlays.here == "switcher" and not device.overlays.switcher_revealed
    layers {
        stack(gap: .tight, fill) {
            if bar_shows(s) {
                bar_on_edge(s, traditional)
            }
            if resting {
                // What the style shows keeps the whole band under the bar, and the banner floats
                // over it: a notice rising never moves what the person had on screen.
                layers(fill) {
                    content
                    at_rest()
                }
            } else if device.overlays.here == "switcher" {
                surface(fill)
            } else if device.overlays.here == "emoji" {
                // The picker stands over the windows: this layer draws nothing of the style, so
                // the windows stay live behind its card, which is the only thing that takes the
                // keys and the pointer while it is up.
                layers(fill) {
                    emoji_card()
                }
            } else if device.overlays.here == "about" {
                // The About sheet stands over the windows, a question the person asked.
                about_sheet()
            } else if device.overlays.here == "peek" {
                // The keys a held Super reaches stand over the windows. The shell's own work
                // stays behind them, so nothing of the style is drawn here; the card itself
                // stands over everything in `shell_around`.
                at_rest()
            } else {
                scroll(fill) {
                    surface()
                }
            }
            if dock_takes_room {
                row(align: .center, distribute: .center) {
                    dock_ground(s, .bottom)
                }
            }
        }
        if dock_shows(s) and not dock_takes_room {
            if s.dock_edge == .bottom {
                row(at: .bottom, fill, align: .center, distribute: .center) {
                    dock_ground(s, .bottom)
                }
            } else if s.dock_edge == .top {
                row(at: .top, fill, align: .center, distribute: .center) {
                    dock_ground(s, .top)
                }
            } else if s.dock_edge == .left {
                // A dock at a leading or trailing edge hugs its own width: the layer
                // sits at that edge (`at: .leading`), centred up it, and a `fill` here would
                // span the whole screen and centre the dock on it instead.
                stack(at: .leading, align: .center) {
                    dock_ground(s, .left)
                }
            } else {
                stack(at: .trailing, align: .center) {
                    dock_ground(s, .right)
                }
            }
        }
        // The hardware keys' answer stands over all of it, at the foot of the screen, for the
        // moment after the last key.
        osd_readout()
        // The keys a held Super reaches, said over everything and in the way of nothing: a
        // picture, so the chord the person is reaching for still runs.
        if let p = device.overlays.peek {
            peek_card(p)
        }
        // Reporting a problem takes the whole screen: the windows stand as cards and the person
        // picks what the report is about, so the picker takes the pointer until it is answered.
        if s.reporting {
            report_picker()
        }
    }
}

/// The room between the bar and the foot that the layout gets: how wide the shell's
/// bar lists actions decides whether each frame's own bar gives way to it (`bar_lists_actions`).
fn shell_lists(traditional: Bool, roomy: Bool) -> Bool {
    bar_lists_actions_for(shell, roomy, traditional)
}

/// The shell the person's style draws its places through, with `traditional` for the desktop
/// style's chrome and `body` the places: every style that shows the layout beside the
/// shell's bar and dock draws through this, so no style repeats the chrome.
template view style_shell(a: Layout, traditional: Bool, body: View) {
    shell_around(a, traditional, body)
}

/// Strips
///
/// The person's places as windows in rows and strips, scrolled sideways (`strips_view`).
view strips(a: Layout) {
    let lists = shell_lists(false, space.width_class == .expanded or space.width_class == .wide)
    style_shell(a, false) {
        strips_view(a, lists, fill)
    }
}

/// Frames
///
/// The places at points on a pannable, zoomable canvas, each in its own frame.
view frames(a: Layout) {
    let lists = shell_lists(false, space.width_class == .expanded or space.width_class == .wide)
    style_shell(a, false) {
        canvas(a, "Canvas", true, lists, fill)
    }
}

/// Frameless
///
/// The places at points, each drawn as its own view shown as content.
view frameless(a: Layout) {
    style_shell(a, false) {
        canvas(a, "Frameless", false, false, fill)
    }
}

/// Desktop
///
/// Frames floating over the shell under the traditional bar, the focused window's commands in it,
/// on a roomy display; a compact output shows the window in front alone in the shell's own
/// chrome, as a phone does.
view desktop(a: Layout) {
    let lists = shell_lists(true, space.width_class == .expanded or space.width_class == .wide)
    if not space.compact {
        layers(fill) {
            traditional_desktop(a, lists, fill)
            // The shell under the frames, its bar the traditional one, which lists the window in
            // front's commands and offers no style switch of its own: the style the person
            // switches by is the bar's of every other style, and the desktop's own bar is this
            // style's.
            shell_around(.none, true) {
            }
        }
    } else {
        style_shell(a, true) {
            front_window(a, lists, fill)
        }
    }
}

/// The style the person shows, as the session found it (`Viewing.style`, a package and a
/// view): the strips when their document names none, so a shell that has never picked one opens.
fn style_of(v: Viewing) -> Text {
    if v.style != "" { v.style } else { "so.tilo.shell:strips" }
}

/// The dock's ground on `edge`: the theme's material for the style the person picked, edge to
/// edge along a top or a foot and inset with rounded corners on a floating dock, which `edge`
/// as `ui.Chrome` names it. What stands on it is the dock's own run (`dock`).
view dock_ground(s: Shell, edge: Edge) {
    if edge == .top or edge == .bottom {
        ui.Chrome(
            name: "Dock",
            shape: if s.dock_fit == .full { .square } else { .rounded },
            edge: if edge == .top { .top } else { .bottom },
            material: if s.dock_finish == .translucent { .glass } else { .calm },
            fill: if s.dock_fit == .full { 1 } else { 0 },
        ) {
            dock(s)
        }
    } else {
        ui.Chrome(
            name: "Dock",
            shape: if s.dock_fit == .full { .square } else { .rounded },
            edge: if edge == .left { .leading } else { .trailing },
            material: if s.dock_finish == .translucent { .glass } else { .calm },
        ) {
            dock(s)
        }
    }
}

/// The top bar on the edge it rides: the whole width with its parts spread along it at the `full`
/// fit, or an inset run centred on the edge at the floating one, where it covers less of what is
/// under it. Its ground is the theme's material for the bar's style either way; how much of the
/// output it spans is what its fit says. `traditional` draws the desktop style's bar, which lists
/// the window in front's commands (`top_bar`).
view bar_on_edge(s: Shell, traditional: Bool) {
    let on_top = s.bar_edge == .top
    // The stack holds the bar at its own height: the bar fills the shell's width, and takes no
    // share of its height, so the shell's windows get the room the bar leaves.
    stack {
        if s.bar_fit == .full and on_top {
            row(at: .top, fill, align: .center) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .square,
                    edge: .top,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                    fill,
                ) {
                    top_bar(traditional)
                }
            }
        } else if s.bar_fit == .full {
            row(at: .bottom, fill, align: .center) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .square,
                    edge: .bottom,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                    fill,
                ) {
                    top_bar(traditional)
                }
            }
        } else if on_top {
            row(at: .top, align: .center, padding: .related) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .rounded,
                    edge: .top,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                ) {
                    top_bar(traditional)
                }
            }
        } else {
            row(at: .bottom, align: .center, padding: .related) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .rounded,
                    edge: .bottom,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                ) {
                    top_bar(traditional)
                }
            }
        }
    }
}

/// The bar's parts in the person's order, as the run the person reorders by dragging a part:
/// what the shell's bar and a window's own bar both draw. The traditional desktop shows its
/// own bar instead (`traditional.tl`), where the focused window's commands stand beside the
/// parts the person chose, so a person picks between the two by picking the style.
///
/// `traditional` is the desktop style's chrome: its bar lists the focused window's commands
/// whatever the chrome's own preset, so `top_bar` draws `traditional_bar`.
view top_bar(traditional: Bool) {
    if traditional or bar_is_traditional(shell) {
        traditional_bar()
    } else {
        bar()
    }
}

/// The banner a notice rose as, floated over what the style shows at rest. A person with windows
/// sees them through the compositor behind this, and who is here is in the bar's own menu, so the
/// shell at rest adds nothing to its bar's band: closed, it lays nothing out and draws nothing.
/// A banner stands the shell's gutter in from the window's edge, as a surface does: its card is
/// rounded, and a rounded card on the edge has its corners cut by it.
view at_rest() {
    if (device.notifications.banner ?? -1) != -1 {
        stack(at: .top, fill, padding: .group) {
            banner()
        }
    }
}

/// The surface the person is in, at the place its kind shows: the launcher fills the room
/// under the bar, the overview and the workspaces fill it, and the centre sits at the
/// trailing edge. Every one of them stands the shell's gutter in from the
/// window's edge, where that window is narrow enough to have no page gutter of its own: a
/// surface is a panel over the windows, not a band of the window.
view surface() {
    stack(fill, padding: if space.compact { .section } else { .none }) {
        if device.overlays.here == "overview" {
            overview(fill)
        } else if device.overlays.here == "switcher" {
            switcher(fill)
        } else if device.overlays.here == "launcher" {
            launcher(fill)
        } else if device.overlays.here == "centre" {
            notices(fill)
        } else if device.overlays.here == "quick-settings" {
            quick_settings(fill)
        } else if device.overlays.here == "workspaces" {
            workspaces(fill)
        }
    }
}
