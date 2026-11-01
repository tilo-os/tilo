/// The actions every surface of the shell asks the session for. Each is the session's own
/// action, run through the desk's `session.run` capability (or a notification's through
/// `session.notify`), so it is attributed, permission checked and undoable as any action is;
/// the shell only names them and lays them out. A run's payload is `(action, arguments)`, the
/// session's own vocabulary, written once here.
/// Brings `key`'s workspace to the front, unless it is there already.
action switch_to_key(key: Text, current: Bool) (icon: .layers, label: "Switch to") {
    require not current "It is in front already"
    outside("session.switch_workspace", key)
}

/// Brings the window `w` to the front: its workspace, its strip, and it.
action focus_window(w: Win) (icon: .arrow_right, label: "Go there", group: "Window") {
    outside("session.run", ("focus-window", w.key))
}

/// Closes the window `w`.
destructive action close_window(w: Win) (icon: .x, label: "Close", group: "Window") {
    outside("session.run", ("close-window", w.key))
}

/// Carries the window `w` onto the strip `strip` of its workspace.
action move_to_strip(w: Win, strip: Text) (icon: .arrow_right, label: "Move to a strip", group: "Window") {
    outside("session.run", ("move-window-to-strip", w.key, strip))
}

/// Carries the window `w` into the workspace `workspace`.
action move_to_room(w: Win, workspace: Text) (icon: .layers, label: "Move to a workspace", group: "Window") {
    outside("session.run", ("move-window-to-workspace", w.key, workspace))
}

/// Searches the whole workspace, what apps and files hold, for `query`: the platform's search,
/// which the overview's window search is not.
action submit_search(query: Text) (icon: .search, label: "Search everything") {
    outside("session.run", ("search", query))
}

/// Opens a person's menu: follow them, message them, see what they are working on.
action show_person(key: Text) (icon: .user, label: "Their menu") {
    outside("session.run", ("show-person", key))
}

/// Opens an agent's menu: see what it is doing and undo what it did.
action show_agent(key: Text) (icon: .bot, label: "Its menu") {
    outside("session.run", ("show-agent", key))
}

/// Asks an agent about the window `w`: its title, the app it shows and what it holds.
action ask_about(w: Win) (icon: .bot, label: "Ask an agent", group: "Window") {
    outside("session.run", ("ask-about-window", w.key))
}

/// Tells the session which surface the desk shows.
action show_surface(wanted: Text) (icon: .layers, label: "Show") {
    outside("session.run", ("show-surface", wanted))
}

/// Closes the surface `wanted`, if it is the one the desk shows.
action close_surface(wanted: Text) (icon: .x, label: "Close") {
    outside("session.run", ("close-surface", wanted))
}

/// Follows `who`: opens what they are on, in the person's shell.
action follow(who: Text) (icon: .eye, label: "Follow") {
    outside("session.run", ("follow", who))
}

/// Stops following whoever the person follows.
action stop_following() (icon: .eye_off, label: "Stop following") {
    outside("session.run", ("stop-following", ""))
}

/// Opens a chat with `who`, in a window of its own beside the one in front.
action message(who: Text) (icon: .message_circle, label: "Message") {
    outside("session.run", ("message", who))
}

/// Calls `who`, in the workspace's call.
action call(who: Text) (icon: .phone, label: "Call") {
    outside("session.run", ("call", who))
}

/// Shows what `who` is working on: their windows and their tasks, read-only.
action see_work(who: Text) (icon: .list, label: "What they are working on") {
    outside("session.run", ("see-work", who))
}

/// Runs one of a notification's actions on it.
action notice_action(n: Notice, key: Text) (icon: .check, label: "Act on it") {
    outside("session.notify", ("invoke", n.key, key))
}

/// Closes a notification.
action dismiss_notice(n: Notice) (icon: .x, label: "Dismiss") {
    outside("session.notify", ("dismiss", n.key))
}

/// Clears every notification in the centre, which Undo puts back.
action clear_notices(d: Desk) (icon: .trash, label: "Clear all") {
    outside("session.notify", ("clear", ""))
}

/// Asks the agent `agent` about `subject`, with what was typed.
action send_to_agent(agent: Text, subject: Text, words: Text) (icon: .send, label: "Send") {
    require words != "" "Write what to ask first"
    outside("session.run", ("ask-agent", agent, subject, words))
}

/// Stops the task `key`.
action stop_task(key: Text) (icon: .square, label: "Stop") {
    outside("session.run", ("stop-task", key))
}

/// Opens the task `key`'s log: each step it ran, what it changed and Undo.
action open_task(key: Text) (icon: .list, label: "Its log") {
    outside("session.run", ("open-task", key))
}

/// Makes a workspace `name` and goes there.
action create_room(d: Desk, name: Text) (icon: .plus, label: "New workspace") {
    require name != "" "Name the workspace first"
    outside("session.run", ("new-workspace-named", name))
}

/// Joins the workspace `key` this device may join.
action join_room(d: Desk, key: Text) (icon: .users, label: "Join") {
    outside("session.run", ("join-workspace", key))
}
