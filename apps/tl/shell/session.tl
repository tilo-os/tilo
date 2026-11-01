/// What the shell's surfaces ask of the session are the session's own actions, published on its
/// door and read as the package `so.tilo.session` (`apps/tl/session/surface.toml`): a view
/// presses one directly (`session.show_surface(surface: "launcher")`), as the agent and the REPL
/// call it, attributed, permission checked and undoable as any action is. The shell declares no
/// action of its own around one.
///
/// The window `w` as the session publishes it: its tab's resource, whose own actions focus and
/// close it.
fn window(w: Win) -> session.Window {
    return resource("desktop:tab:{w.key}", of: session.Window)
}

/// The run of the task `key` as the session publishes it, whose own actions steer and stop it.
fn run(key: Text) -> session.AgentRun {
    return resource("agent-run:{key}", of: session.AgentRun)
}

/// The id of a place, strip or row of the arrangement as its actions take it: the number its
/// record's key writes.
fn arranged(key: Text) -> Int {
    return key.to_int() ?? 0
}
