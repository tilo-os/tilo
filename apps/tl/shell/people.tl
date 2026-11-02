/// Who is here, and their menus. A person's presence on the bar (their face, and the chip that
/// names them) opens the session's roster, where each person's card is. An agent's presence
/// opens its own: stopping its run.
///
/// Every entry is an action the session publishes (`so.tilo.session`). Nothing here reads a
/// copy of presence: it reads the desk's people and agents.
///
/// Who else is here, as one door: on a bar with the room it names them, and everywhere it is the
/// people button — the faces beside it are the glance, this opens the roster of everyone here.
view people_button(d: Desk) {
    let people = d.people
    let one = people.first() ?? Person(key: "", name: "People")
    if space.wide and not people.is_empty() {
        // Who is here, in the fewest words that name them all: one name, two joined, or the
        // first and how many others.
        let others = people.len() - 1
        let second = people.last() ?? one
        let label = if others == 0 { "{one.name} is here" } else if others == 1 { "{one.name} and {second.name} are here" } else { "{one.name} and {others} others are here" }
        ui.Button(label, icon: .users, role: .bar) {
            on press: session.toggle_collaborators()
        }
    } else {
        ui.Button("People", icon: .users, icon_only: true, role: .bar) {
            on press: session.toggle_collaborators()
        }
    }
}

/// An agent here, as its presence: a chip that opens its menu.
view agent_chip(d: Desk, a: Agent) {
    ui.MenuButton(a.name, icon: .bot, role: .bar) {
        if a.state != "done" {
            ui.MenuItem("Stop", icon: .square, destructive: true) {
                on choose: session.stop_run(run(a.key))
            }
        }
    }
}

/// An agent here with what it is doing and what it last changed: its face, its activity line
/// and the mark of what it did, which opens the run's log.
view agent_line(d: Desk, a: Agent) {
    ui.ListRow(a.name, secondary: .some(if a.doing != "" { a.doing } else { a.state }), icon: .bot) {
        trailing: {
            ui.Badge(count: a.unseen)
        }
    }
}
