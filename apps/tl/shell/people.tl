/// Who is here, and their menus. Clicking a person's presence anywhere (the bar, a frame, the
/// overview) opens the same menu: follow them, which opens what they are on in this person's
/// shell; message them; call them; see what they are working on and what their tasks and agents
/// are doing. An agent's presence opens its own: what it is doing, what it changed, its log and
/// its run's Undo.
///
/// Every entry is an action the session or the workspace runs (`session.run`, the chat app's
/// own). Nothing here reads a copy of presence: it reads the desk's people and agents.

/// A person here, as their presence: a chip that opens their menu.
view person_chip(p: Person) {
    ui.MenuButton(p.name, icon: .user, role: .plain) {
        ui.MenuItem("Follow", icon: .eye) {
            on choose: follow(p.key)
        }
        ui.MenuItem("Stop following", icon: .eye_off) {
            on choose: stop_following()
        }
        ui.MenuItem("Message", icon: .message_circle) {
            on choose: message(p.key)
        }
        ui.MenuItem("Call", icon: .phone) {
            on choose: call(p.key)
        }
        ui.MenuItem("What they are working on", icon: .list) {
            on choose: see_work(p.key)
        }
        ui.MenuItem("Their tasks and agents", icon: .bot) {
            on choose: see_work(p.key)
        }
    }
}

/// An agent here, as its presence: a chip that opens its menu.
view agent_chip(a: Agent) {
    ui.MenuButton(a.name, icon: .bot, role: .plain) {
        ui.MenuItem("What it is doing", icon: .list) {
            on choose: show_agent(a.key)
        }
        ui.MenuItem("Its log", icon: .history) {
            on choose: open_task(a.key)
        }
        if a.state != "done" {
            ui.MenuItem("Stop", icon: .square, destructive: true) {
                on choose: stop_task(a.key)
            }
        }
    }
}

/// Someone here with what they are on: their face, their name, and the resource they are
/// looking at, which opens there.
view person_line(p: Person, doing: Text) {
    ui.ListRow(p.name, secondary: .some(doing), icon: .user) {
        trailing: {
            ui.Button("Follow", icon: .eye, icon_only: true) {
                on press: follow(p.key)
            }
        }
        on activate: follow(p.key)
    }
}

/// An agent here with what it is doing and what it last changed: its face, its activity line
/// and the mark of what it did, which opens the run's log.
view agent_line(a: Agent) {
    ui.ListRow(a.name, secondary: .some(if a.doing != "" { a.doing } else { a.state }), icon: .bot) {
        trailing: {
            ui.Badge(count: a.unseen)
        }
        on activate: open_task(a.key)
    }
}

/// Who is here in the workspace in front, and what the person's agents are doing, each opening
/// its menu. The desk shows it at rest, and the overview's strips show the same people on the
/// windows they are on.
view here_now(d: Desk) {
    if not d.people.is_empty() or not d.agents.is_empty() {
        ui.Section("Here") {
            row(gap: .related, wrap: true) {
                for p in d.people {
                    person_chip(p)
                }
                for a in d.agents.filter(a -> a.state != "done") {
                    agent_chip(a)
                }
            }
        }
    }
}
