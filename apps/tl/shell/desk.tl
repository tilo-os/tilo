/// The desk: what this device shows. The bar across the top, and one surface over it at a
/// time: the overview, the switcher, search and launching, the notifications, the workspaces
/// and the composer. Which one is up is the session's (`d.here`), so the shell re-runs when it
/// changes and nothing else moves while the desk is at rest.
///
/// Every surface is drawn over the bar's ground, so the bar stays reachable and a press outside
/// a surface closes it, as the session's chrome decides. Nothing here draws a window: the
/// desk's windows are the compositor's, behind this view.
view desk(d: Desk) {
    ui.Scroll {
        stack(gap: .tight) {
            bar(d)
            surface_or_rest(d)
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

/// The desk at rest under its bar: where the person is, the banner a notice rose as, and who is
/// here. A person with windows sees them through the compositor behind this.
view at_rest(d: Desk) {
    stack(gap: .section) {
        desktop_hint(d)
        if d.banner != "" {
            banner(d)
        }
        here_now(d)
    }
}

/// What the desk says under its bar at rest: where the person is, and how many tasks run.
view desktop_hint(d: Desk) {
    row(padding: .group, gap: .related, align: .center, wrap: true) {
        ui.Icon(.app_window, size: .small, secondary: true)
        ui.Text(if d.strip != "" { d.strip } else { d.workspace }, role: .caption, secondary: true)
        spacer
        if not d.tasks.is_empty() {
            ui.Badge(count: d.tasks.count(t -> t.state != "done"))
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
