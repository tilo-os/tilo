/// The desk: what this device shows. The bar across the top, and one surface over it at a
/// time: the overview, the switcher, search and launching, the notifications, the workspaces
/// and the composer. Which one is up is the session's (`d.here`), so the shell re-runs when it
/// changes and nothing else moves while the desk is at rest.
///
/// Every surface is drawn over the bar's ground, so the bar stays reachable and a press outside
/// a surface closes it, as the session's chrome decides. Nothing here draws a window: the
/// desk's windows are the compositor's, behind this view.
view desk(d: Desk) {
    let s = shell
    layers {
        stack(gap: .tight) {
            if bar_shows(s) {
                row(at: .top, fill) {
                    bar(d)
                }
            }
            surface_or_rest(d)
        }
        if dock_shows(s) {
            dock_body(s, d)
        }
    }
}

/// What stands under the bar: the desk at rest, or the surface the person is in.
view surface_or_rest(d: Desk) {
    if d.here == "none" or d.here == "desk" {
        at_rest(d)
    } else {
        surface(d)
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
