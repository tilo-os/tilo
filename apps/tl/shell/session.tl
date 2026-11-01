/// The actions every surface of the shell asks the session for. Each is the session's own
/// action, run through the desk's `session.run` capability (or a notification's through
/// `session.notify`), so it is attributed, permission checked and undoable as any action is;
/// the shell only names them and lays them out. A run's payload is the invocation as every
/// caller of the action protocol addresses one: `(resource, action, [(argument, value)…])`, the
/// resource and the action as the session's door publishes them and each argument by the name
/// the action's own field has. The checker holds every `session.run` to that shape.
///
/// What the session's door does not publish yet (an agent's menu, asking about a window,
/// following, messaging, calling and seeing a person's work, a task's log) is addressed to the
/// desk, which refuses it as an action it lacks: the press reaches the session, and the refusal
/// is the shell's to show, never a quiet drop.
///
/// The address of the window `w` on the session's door: its tab's resource.
fn window(w: Win) -> Text {
    return "desktop:tab:{w.key}"
}

/// Brings `key`'s workspace to the front, unless it is there already.
action switch_to_key(key: Text, current: Bool) (icon: .layers, label: "Switch to") {
    require not current "It is in front already"
    outside("session.switch_workspace", key)
}

/// Brings the window `w` to the front: its workspace, its strip, and it.
action focus_window(w: Win) (icon: .arrow_right, label: "Go there", group: "Window") {
    outside("session.run", (window(w), "focus", []))
}

/// Closes the window `w`.
destructive action close_window(w: Win) (icon: .x, label: "Close", group: "Window") {
    outside("session.run", (window(w), "close", []))
}

/// Carries the window `w` onto the strip `strip` of its workspace.
action move_to_strip(w: Win, strip: Text) (icon: .arrow_right, label: "Move to a strip", group: "Window") {
    outside("session.run", ("desktop:chrome", "move-tab-to-strip", [("tab", w.key), ("strip", strip)]))
}

/// Carries the window `w` into the workspace `workspace`.
action move_to_room(w: Win, workspace: Text) (icon: .layers, label: "Move to a workspace", group: "Window") {
    outside("session.run", ("desktop:chrome", "move-tab", [("tab", w.key), ("workspace", workspace)]))
}

/// Searches the whole workspace, what apps and files hold, for `query`: the platform's search,
/// which the overview's window search is not.
action submit_search(query: Text) (icon: .search, label: "Search everything") {
    outside("session.run", ("desktop:desk", "open-search", [("query", query)]))
}

/// Opens a person's menu: follow them, message them, see what they are working on.
action show_person(key: Text) (icon: .user, label: "Their menu") {
    outside("session.run", ("desktop:chrome", "show-person", [("person", key)]))
}

/// Opens an agent's menu: see what it is doing and undo what it did.
action show_agent(key: Text) (icon: .bot, label: "Its menu") {
    outside("session.run", ("session:desk", "show-agent", [("agent", key)]))
}

/// Asks an agent about the window `w`: its title, the app it shows and what it holds.
action ask_about(w: Win) (icon: .bot, label: "Ask an agent", group: "Window") {
    outside("session.run", ("session:desk", "ask-about-window", [("tab", w.key)]))
}

/// Tells the session which surface the desk shows.
action show_surface(wanted: Text) (icon: .layers, label: "Show") {
    outside("session.run", ("session:desk", "show-surface", [("surface", wanted)]))
}

/// Closes the surface `wanted`, if it is the one the desk shows.
action close_surface(wanted: Text) (icon: .x, label: "Close") {
    outside("session.run", ("session:desk", "close-surface", [("surface", wanted)]))
}

/// Follows `who`: opens what they are on, in the person's shell.
action follow(who: Text) (icon: .eye, label: "Follow") {
    outside("session.run", ("session:desk", "follow", [("person", who)]))
}

/// Stops following whoever the person follows.
action stop_following() (icon: .eye_off, label: "Stop following") {
    outside("session.run", ("session:desk", "stop-following", []))
}

/// Opens a chat with `who`, in a window of its own beside the one in front.
action message(who: Text) (icon: .message_circle, label: "Message") {
    outside("session.run", ("session:desk", "message", [("person", who)]))
}

/// Calls `who`, in the workspace's call.
action call(who: Text) (icon: .phone, label: "Call") {
    outside("session.run", ("session:desk", "call", [("person", who)]))
}

/// Shows what `who` is working on: their windows and their tasks, read-only.
action see_work(who: Text) (icon: .list, label: "What they are working on") {
    outside("session.run", ("session:desk", "see-work", [("person", who)]))
}

/// Runs one of a notification's choices: the desk's `act-on-notice`, naming the notice and the
/// key its sender gave the choice. What any action on the desk is: attributed, checked and
/// undoable.
action notice_action(n: Notice, key: Text) (icon: .check, label: "Act on it") {
    outside("session.notify", ("session:desk", "act-on-notice", [("notice", n.key), ("action", key)]))
}

/// Closes a notification: the desk's `dismiss-notice`.
action dismiss_notice(n: Notice) (icon: .x, label: "Dismiss") {
    outside("session.notify", ("session:desk", "dismiss-notice", [("notice", n.key)]))
}

/// Clears every notification in the centre, which Undo puts back: the desk's `clear-notices`.
action clear_notices(d: Desk) (icon: .trash, label: "Clear all") {
    outside("session.notify", ("session:desk", "clear-notices", []))
}

/// Asks the agent `agent` about `subject`, with what was typed: the session's runs' `ask`.
action send_to_agent(agent: Text, subject: Text, words: Text) (icon: .send, label: "Send") {
    require words != "" "Write what to ask first"
    outside("session.run", ("agent-runs", "ask", [("agent", agent), ("title", words), ("subject", subject)]))
}

/// Stops the task `key`: its run's `stop-run`.
action stop_task(key: Text) (icon: .square, label: "Stop") {
    outside("session.run", ("agent-run:{key}", "stop-run", []))
}

/// Opens the task `key`'s log: each step it ran, what it changed and Undo.
action open_task(key: Text) (icon: .list, label: "Its log") {
    outside("session.run", ("session:desk", "open-task", [("task", key)]))
}

/// Makes a workspace `name` and goes there.
action create_room(d: Desk, name: Text) (icon: .plus, label: "New workspace") {
    require name != "" "Name the workspace first"
    outside("session.run", ("desktop:chrome", "new-workspace", [("name", name)]))
}

/// Joins the workspace `key` this device may join: a workspace of its own, sharing that one.
action join_room(d: Desk, key: Text) (icon: .users, label: "Join") {
    outside("session.run", ("desktop:chrome", "new-workspace", [("room", key)]))
}
