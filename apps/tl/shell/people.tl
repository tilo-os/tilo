/// Who is here, and their menus. Clicking a person's presence anywhere (the bar, a frame, the
/// overview) opens the session's menu of them. An agent's presence opens its own: stopping its
/// run.
///
/// Every entry is an action the session publishes (`so.tilo.session`). Nothing here reads a
/// copy of presence: it reads the desk's people and agents.
/// A person here, as their presence: a chip that opens their menu.
view person_chip(d: Desk, p: Person) {
    ui.Button(p.name, icon: .user, role: .plain) {
        on press: session.show_person(person: p.key)
    }
}

/// An agent here, as its presence: a chip that opens its menu.
view agent_chip(d: Desk, a: Agent) {
    ui.MenuButton(a.name, icon: .bot, role: .plain) {
        if a.state != "done" {
            ui.MenuItem("Stop", icon: .square, destructive: true) {
                on choose: session.stop_run(run(a.key))
            }
        }
    }
}

/// Someone here with what they are on: their face, their name, and the resource they are
/// looking at, which opens there.
view person_line(p: Person, doing: Text) {
    ui.ListRow(p.name, secondary: .some(doing), icon: .user) {
        on activate: session.show_person(person: p.key)
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
