/// What the shell's surfaces ask of the session are the session's own actions, published on its
/// door and read as packages: the desktop's windows, workspaces and arrangement as
/// `so.tilo.session` (`apps/tl/session/surface.toml`), and the device's resources as
/// `so.tilo.device`. A view presses one directly (`session.focus(window(w))`,
/// `device.show_surface(device.overlays, surface: "launcher")`), as the agent and the REPL call it,
/// attributed, permission checked and undoable as any action is. The shell declares no action
/// of its own around one.
///
/// The window `w` as the session publishes it: its tab's resource, whose own actions focus and
/// close it.
fn window(w: device.Window) -> session.Window {
    return resource("desktop:tab:{w.key}", of: session.Window)
}

/// The place `key` of the arrangement as the session publishes it, whose own actions pin, close
/// and move it. A place, a strip and a row are resources of their own, so a press names the
/// thing it presses and no key is a number.
fn the_place(key: Text) -> session.Place {
    return resource("shell:place:{key}", of: session.Place)
}

/// The strip `key` of the arrangement as the session publishes it, whose own action resizes it.
fn the_strip(key: Text) -> session.Strip {
    return resource("shell:strip:{key}", of: session.Strip)
}

/// The row `key` of the arrangement as the session publishes it: what a place's move names as
/// where the place goes.
fn the_row(key: Text) -> session.Row {
    return resource("shell:row:{key}", of: session.Row)
}

/// Show `found` in the place `place` while it shows a query: the same place, now the found
/// resource (shell.md §8).
action query_show(place: Text, found: host.Resource) {
    session.show(place: the_place(place), resource: found)
}

/// Look for what the place `place`'s search names again.
action query_retry(place: Text) {
    session.retry_query(place: the_place(place))
}

/// The id of the workspace in front: the one whose windows the switcher walks and the overview
/// shows as the layout's.
fn front_workspace() -> Text {
    if let w = arrangement.workspaces.find(w -> w.current) {
        return w.key
    }
    return ""
}

/// The name of the workspace in front, as the bar names it.
fn front_workspace_name() -> Text {
    if let w = arrangement.workspaces.find(w -> w.current) {
        return w.name
    }
    return ""
}

/// The name of the row in front in the workspace in front: the strip the bar names after the
/// workspace.
fn front_strip_name() -> Text {
    let w = front_workspace()
    if let r = arrangement.rows.find(r -> r.workspace == w and r.current) {
        return r.name
    }
    return ""
}
