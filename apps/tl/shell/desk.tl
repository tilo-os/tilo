/// The desk: what this device shows. The person's bar across the top edge its settings name,
/// the dock on its own edge, and one surface over the desk at a time: the overview, the
/// switcher, search and launching, the notifications, the workspaces and the composer. Which
/// one is up is the session's (`d.here`), so the shell re-runs when it changes and nothing else
/// moves while the desk is at rest.
///
/// The bar and the dock are `ui.Chrome` bodies over the theme's bar material: edge to edge and
/// square at the `full` fit, an inset rounded body at the floating one. Every surface is drawn
/// over them, so the bar stays reachable and a press outside a surface closes it, as the
/// session's chrome decides. Nothing here draws a window: the desk's windows are the
/// compositor's, behind this view.
view desk(d: Desk) {
    let s = shell
    layers {
        stack(gap: .tight, fill) {
            if bar_shows(s, d) {
                bar_on_edge(s, d)
            }
            if d.here == "none" or d.here == "desk" {
                at_rest(d)
            } else {
                scroll(fill) {
                    surface(d)
                }
            }
        }
        if dock_shows(s, d) {
            if s.dock_edge == .bottom {
                row(at: .bottom, fill, align: .center, distribute: .center) {
                    ui.Chrome(name: "Dock", shape: if s.dock_fit == .full { .square } else { .rounded }, edge: .bottom, fill: if s.dock_fit == .full { 1 } else { 0 }) {
                        dock(s, d)
                    }
                }
            } else if s.dock_edge == .top {
                row(at: .top, fill, align: .center, distribute: .center) {
                    ui.Chrome(name: "Dock", shape: if s.dock_fit == .full { .square } else { .rounded }, edge: .top, fill: if s.dock_fit == .full { 1 } else { 0 }) {
                        dock(s, d)
                    }
                }
            } else if s.dock_edge == .left {
                stack(at: .leading, fill, align: .center) {
                    ui.Chrome(name: "Dock", shape: if s.dock_fit == .full { .square } else { .rounded }, edge: .leading) {
                        dock(s, d)
                    }
                }
            } else {
                stack(at: .trailing, fill, align: .center) {
                    ui.Chrome(name: "Dock", shape: if s.dock_fit == .full { .square } else { .rounded }, edge: .trailing) {
                        dock(s, d)
                    }
                }
            }
        }
    }
}

/// The top bar on the edge it rides: the whole width with its parts spread along it at the
/// `full` fit, an inset run centred on the edge at the floating one — where it covers less of
/// what is under it. Its ground is the theme's bar material either way; how much of the output
/// it spans is what its fit says.
view bar_on_edge(s: Shell, d: Desk) {
    let on_top = s.bar_edge == .top
    if s.bar_fit == .full and on_top {
        row(at: .top, fill, align: .center) {
            ui.Chrome(name: "Top bar", shape: .square, edge: .top, fill) {
                top_bar(d)
            }
        }
    } else if s.bar_fit == .full {
        row(at: .bottom, fill, align: .center) {
            ui.Chrome(name: "Top bar", shape: .square, edge: .bottom, fill) {
                top_bar(d)
            }
        }
    } else if on_top {
        row(at: .top, align: .center, padding: .related) {
            ui.Chrome(name: "Top bar", shape: .rounded, edge: .top) {
                top_bar(d)
            }
        }
    } else {
        row(at: .bottom, align: .center, padding: .related) {
            ui.Chrome(name: "Top bar", shape: .rounded, edge: .bottom) {
                top_bar(d)
            }
        }
    }
}

/// The bar's parts in the person's order, as the run the person reorders by dragging a part:
/// what the desk's bar and the desk window's bar both draw. The traditional desktop shows its
/// own bar instead (`traditional.tl`), where the focused window's commands stand beside the
/// parts the person chose, so a person picks between the two by picking the preset.
view top_bar(d: Desk) {
    if shell.based_on == .windows_desktop and shell.preset == .windows_desktop {
        traditional_bar(d)
    } else {
        bar(d)
    }
}

/// The desk at rest under its bar: the banner a notice rose as. A person with windows sees
/// them through the compositor behind this, and who is here is in the bar's own menu, so the
/// desk at rest adds nothing to its bar's band: closed, it lays nothing out and draws nothing.
view at_rest(d: Desk) {
    stack(gap: .section) {
        if d.banner != "" {
            banner(d)
        }
    }
}

/// The surface the person is in, at the place its kind shows: the launcher fills the desk
/// under the bar, the overview and the workspaces fill it, the centre sits at the trailing
/// edge and the composer at the foot.
view surface(d: Desk) {
    if d.here == "overview" {
        overview(d, fill)
    } else if d.here == "switcher" {
        switcher(d, fill)
    } else if d.here == "launcher" {
        launcher(d, fill)
    } else if d.here == "centre" {
        notices(d, fill)
    } else if d.here == "workspaces" {
        workspaces(d, fill)
    } else if d.here == "composer" {
        composer(d, fill)
    }
}
