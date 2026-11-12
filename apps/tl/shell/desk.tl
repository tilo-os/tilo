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
    desk_around(d, .none, false) {
    }
}

/// The desk, with the arrangement `front` filling the room between its bar and its foot, as the
/// style the person chose shows it (`screen`): the dock stands over that room on its own edge,
/// as the chrome's does, so a window is as tall as the desk rather than stopping short of the
/// dock. On a compact output that room is a phone's, too little for a window's bar to lose to a
/// floating dock, so the dock takes its own room at the foot instead and the window fills what
/// is left.
///
/// The switcher is a panel over the desk rather than a page of it: it takes that room and
/// centres itself there, where the surfaces that go on past a display are given the same room
/// as a page that scrolls. Until the session says its walk outlasted a flick
/// (`switcher_revealed`) the desk is the one at rest, so a flick between two windows shows
/// nothing over either.
///
/// `traditional` makes this the traditional desktop's chrome: its bar lists the window in
/// front's actions, and its own bar is drawn (`top_bar`). The desktop style passes it.
template view desk_around(d: Desk, front: Option<Arrangement>, traditional: Bool, content: View) {
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
                bar_on_edge(s, d, traditional)
            }
            if resting {
                stack(gap: .tight, fill) {
                    at_rest(d)
                    content
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

/// The room the room between the bar and the foot gives the arrangement: how wide the desk's
/// bar lists actions decides whether each frame's own bar gives way to it (`bar_lists_actions`).
fn desk_lists(traditional: Bool, roomy: Bool) -> Bool {
    bar_lists_actions_for(shell, root Desk, roomy, traditional)
}

/// The desk the person's style draws its places through, with `traditional` for the desktop
/// style's chrome and `body` the places: every style that shows the arrangement beside the
/// desk's bar and dock draws through this, so no style repeats the chrome.
template view desk_style(a: Arrangement, traditional: Bool, body: View) {
    desk_around(root Desk, a, traditional, body)
}

/// Strips
///
/// The person's places as windows in rows and strips, scrolled sideways (`strips_view`).
view strips(a: Arrangement) {
    let lists = desk_lists(false, space.width_class == .expanded or space.width_class == .wide)
    desk_style(a, false) {
        strips_view(a, viewing, lists, fill)
    }
}

/// Frames
///
/// The places at points on a pannable, zoomable canvas, each in its own frame.
view frames(a: Arrangement) {
    let lists = desk_lists(false, space.width_class == .expanded or space.width_class == .wide)
    desk_style(a, false) {
        canvas(a, "Canvas", true, lists, fill)
    }
}

/// Spaces
///
/// The places at points, each drawn as its own view standing as content.
view spaces(a: Arrangement) {
    desk_style(a, false) {
        canvas(a, "Spaces", false, false, fill)
    }
}

/// Desktop
///
/// Frames on a floating desk under the traditional bar, the focused window's commands in it,
/// on a roomy display; a compact output shows the window in front alone in the desk's own
/// chrome, as a phone does.
view desktop(a: Arrangement) {
    let lists = desk_lists(true, space.width_class == .expanded or space.width_class == .wide)
    if not space.compact {
        layers(fill) {
            traditional_desktop(a, lists, fill)
            // The desk under the frames, its bar the traditional one, which lists the window in
            // front's commands and offers no style switch of its own: the style the person
            // switches by is the bar's of every other style, and the desktop's own bar is this
            // style's.
            desk_around(root Desk, .none, true) {
            }
        }
    } else {
        desk_style(a, true) {
            front_window(a, lists, fill)
        }
    }
}

/// The desk style the person shows, as the session found it (`Viewing.style`, a package and a
/// view): the strips when their record names none, so a shell that has never picked one opens.
fn style_of(v: Viewing) -> Text {
    if v.style != "" { v.style } else { "so.tilo.shell:strips" }
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

/// The top bar on the edge it rides: the whole width with its parts spread along it at the `full`
/// fit, or an inset run centred on the edge at the floating one, where it covers less of what is
/// under it. Its ground is the theme's material for the bar's style either way; how much of the
/// output it spans is what its fit says. `traditional` draws the desktop style's bar, which lists
/// the window in front's commands (`top_bar`).
view bar_on_edge(s: Shell, d: Desk, traditional: Bool) {
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
                    top_bar(d, traditional)
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
                    top_bar(d, traditional)
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
                    top_bar(d, traditional)
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
                    top_bar(d, traditional)
                }
            }
        }
    }
}

/// The bar's parts in the person's order, as the run the person reorders by dragging a part:
/// what the desk's bar and the desk window's bar both draw. The traditional desktop shows its
/// own bar instead (`traditional.tl`), where the focused window's commands stand beside the
/// parts the person chose, so a person picks between the two by picking the style.
///
/// `traditional` is the desktop style's chrome: its bar lists the focused window's commands
/// whatever the chrome's own preset, so `top_bar` draws `traditional_bar`.
view top_bar(d: Desk, traditional: Bool) {
    if traditional or bar_is_traditional(shell) {
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
