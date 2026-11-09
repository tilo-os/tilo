/// The desk: what this device shows. The person's bar across the top edge its settings name,
/// the dock on its own edge, and one surface over the desk at a time: the overview, the
/// switcher, search and launching, the notifications and the workspaces. Which
/// one is up is the session's (`d.here`), so the shell re-runs when it changes and nothing else
/// moves while the desk is at rest.
///
/// The bar and the dock are `ui.Chrome` bodies made of the theme's material for the style the
/// person picked (`bar_finish`, `dock_finish`): the theme's glass, let through and blurred over
/// the desk, or that same glass made calm, opaque with every light off — so a theme's material
/// and not the view decides what either is made of. Edge to edge and square at the `full` fit,
/// an inset rounded body at the floating one. Every surface is drawn
/// over them, so the bar stays reachable and a press outside a surface closes it, as the
/// session's chrome decides. Nothing here draws a window: the desk's windows are the
/// compositor's, behind this view.
view desk(d: Desk) {
    desk_around(d, .none, fill)
}

/// The desk, with the arrangement `front` filling the room between its bar and its foot, as its
/// preset shows it (`shown_preset`): the dock stands over that room on its own edge, as the
/// chrome's does, so a window is as tall as the desk rather than stopping short of the dock. On a
/// compact output that room is a phone's, too little for a window's bar to lose to a floating
/// dock, so the dock takes its own room at the foot instead and the window fills what is left.
///
/// The switcher is a panel over the desk rather than a page of it: it takes that room and
/// centres itself there, where the surfaces that go on past a display are given the same room
/// as a page that scrolls. Until the session says its walk outlasted a flick
/// (`switcher_revealed`) the desk is the one at rest, so a flick between two windows shows
/// nothing over either.
view desk_around(d: Desk, front: Option<Arrangement>) {
    let s = shell
    let dock_takes_room = front != .none
        and dock_shows(s, d)
        and s.dock_edge == .bottom
        and space.compact
    let resting = d.here == ""
        or d.here == "none"
        or d.here == "desk"
        or d.here == "switcher" and not d.switcher_revealed
    layers {
        stack(gap: .tight, fill) {
            if bar_shows(s, d) {
                bar_on_edge(s, d)
            }
            if resting {
                if let a = front {
                    stack(gap: .tight, fill) {
                        at_rest(d)
                        shown_preset(
                            a,
                            bar_lists_actions(
                                s,
                                d,
                                space.width_class == .expanded or space.width_class == .wide,
                            ),
                            fill,
                        )
                    }
                } else {
                    at_rest(d)
                }
            } else if d.here == "switcher" {
                surface(d, fill)
            } else {
                scroll(fill) {
                    surface(d)
                }
            }
            if dock_takes_room {
                row(align: .center, distribute: .center) {
                    dock_ground(s, d, .bottom)
                }
            }
        }
        if dock_shows(s, d) and not dock_takes_room {
            if s.dock_edge == .bottom {
                row(at: .bottom, fill, align: .center, distribute: .center) {
                    dock_ground(s, d, .bottom)
                }
            } else if s.dock_edge == .top {
                row(at: .top, fill, align: .center, distribute: .center) {
                    dock_ground(s, d, .top)
                }
            } else if s.dock_edge == .left {
                stack(at: .leading, fill, align: .center) {
                    dock_ground(s, d, .left)
                }
            } else {
                stack(at: .trailing, fill, align: .center) {
                    dock_ground(s, d, .right)
                }
            }
        }
    }
}

/// The desk as the whole of a display (`tilo-session --shell screen`): the bar, the dock and the
/// desk's surfaces around the person's arrangement as the preset they picked shows it, each
/// app's window standing in the well its place leaves it, as a window of the desk shows it
/// (`desk_view`). The traditional desktop on a roomy display is its own whole screen, its frames
/// under the desk's bar and dock.
///
/// This is the one definition of the whole screen: the arrangement's default view (`desk_view`)
/// draws it, so a person shows the desk as a window and as a display by the same code, and the
/// session's `screen` layer and a window's `desk_view` never drift apart.
view screen(a: Arrangement) {
    let v = viewing
    if (v.showing == .desktop or bar_is_traditional(shell)) and not space.compact {
        layers(fill) {
            traditional_desktop(
                a,
                bar_lists_actions(
                    shell,
                    root Desk,
                    space.width_class == .expanded or space.width_class == .wide,
                ),
                fill,
            )
            desk(root Desk, fill)
        }
    } else {
        desk_around(root Desk, a, fill)
    }
}

/// The dock's ground on `edge`: the theme's material for the style the person picked, edge to
/// edge along a top or a foot and inset with rounded corners on a floating dock, which `edge`
/// as `ui.Chrome` names it. What stands on it is the dock's own run (`dock`).
view dock_ground(s: Shell, d: Desk, edge: Edge) {
    if edge == .top or edge == .bottom {
        ui.Chrome(
            name: "Dock",
            shape: if s.dock_fit == .full { .square } else { .rounded },
            edge: if edge == .top { .top } else { .bottom },
            material: if s.dock_finish == .translucent { .glass } else { .calm },
            fill: if s.dock_fit == .full { 1 } else { 0 },
        ) {
            dock(s, d)
        }
    } else {
        ui.Chrome(
            name: "Dock",
            shape: if s.dock_fit == .full { .square } else { .rounded },
            edge: if edge == .left { .leading } else { .trailing },
            material: if s.dock_finish == .translucent { .glass } else { .calm },
        ) {
            dock(s, d)
        }
    }
}

/// What stands between the bar and the dock when the desk shows the arrangement: the places as
/// the preset the person picked lays them out, under the switch between the presets, as
/// `desk_view` shows them. The traditional desktop shows the window in front alone here, as a
/// compact display does, its bar's actions giving way when the desk's bar lists them (`lists`).
view shown_preset(a: Arrangement, lists: Bool) {
    let v = viewing
    if v.showing == .desktop {
        front_window(a, lists, fill)
    } else if v.showing == .frames {
        framed_canvas(a, fill)
    } else if v.showing == .spaces {
        spaces_canvas(a, fill)
    } else {
        strips_view(a, v, lists, fill)
    }
}

/// The top bar on the edge it rides: the whole width with its parts spread along it at the `full`
/// fit, or an inset run centred on the edge at the floating one, where it covers less of what is
/// under it. Its ground is the theme's material for the bar's style either way; how much of the
/// output it spans is what its fit says.
view bar_on_edge(s: Shell, d: Desk) {
    let on_top = s.bar_edge == .top
    // The stack holds the bar at its own height: the bar fills the desk's width, and takes no
    // share of its height, so the desk's windows get the room the bar leaves.
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
                    top_bar(d)
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
                    top_bar(d)
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
                    top_bar(d)
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
                    top_bar(d)
                }
            }
        }
    }
}

/// The bar's parts in the person's order, as the run the person reorders by dragging a part:
/// what the desk's bar and the desk window's bar both draw. The traditional desktop shows its
/// own bar instead (`traditional.tl`), where the focused window's commands stand beside the
/// parts the person chose, so a person picks between the two by picking the preset.
///
/// The bar follows the desk's preset (`Viewing.showing`) as well as the chrome's: a desk shown
/// as the traditional desktop gets its bar, whose focused window's commands list what the
/// window's own header had to give way, even before the person applies the chrome preset that
/// makes it their own.
view top_bar(d: Desk) {
    if bar_is_traditional(shell) or viewing.showing == .desktop {
        traditional_bar(d)
    } else {
        bar(d)
    }
}

/// The desk at rest under its bar: the banner a notice rose as. A person with windows sees
/// them through the compositor behind this, and who is here is in the bar's own menu, so the
/// desk at rest adds nothing to its bar's band: closed, it lays nothing out and draws nothing.
/// A banner stands the desk's gutter in from the window's edge, as a surface does.
view at_rest(d: Desk) {
    if d.banner != "" {
        stack(padding: if space.compact { .section } else { .none }) {
            banner(d)
        }
    }
}

/// The surface the person is in, at the place its kind shows: the launcher fills the desk
/// under the bar, the overview and the workspaces fill it, and the centre sits at the
/// trailing edge. Every one of them stands the desk's gutter in from the
/// window's edge, where that window is narrow enough to have no page gutter of its own: a
/// surface is a panel over the desk, not a band of the window.
view surface(d: Desk) {
    stack(fill, padding: if space.compact { .section } else { .none }) {
        if d.here == "overview" {
            overview(d, fill)
        } else if d.here == "switcher" {
            switcher(d, fill)
        } else if d.here == "launcher" {
            launcher(d, fill)
        } else if d.here == "centre" {
            notices(d, fill)
        } else if d.here == "quick-settings" {
            quick_settings(d, fill)
        } else if d.here == "workspaces" {
            workspaces(d, fill)
        }
    }
}
