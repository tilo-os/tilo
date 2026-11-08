/// Who is here, and their menus. A person's presence on the bar is their face in the group
/// that opens the session's roster, where each person's card is. An agent's presence opens its
/// own: stopping its run.
///
/// Every entry is an action the session publishes (`so.tilo.session`). Nothing here reads a
/// copy of presence: it reads the desk's people and agents.
///
/// Who else is here, as one door: the faces alone, up to three and then a face counting the
/// rest, so the group says who is here at a glance and takes no words from the bar. The one
/// button opens the roster, and its name names the people for assistive technology. With
/// nobody here it is the people button's own glyph.
view people_button(d: Desk) {
    let people = d.people
    if people.is_empty() {
        ui.Button("People", icon: .users, icon_only: true, role: .bar) {
            on press: session.toggle_collaborators()
        }
    } else {
        // Who is here, in the fewest words that name them all: one name, two joined, or the
        // first and how many others.
        let one = people.first() ?? Person(key: "", name: "People")
        let others = people.len() - 1
        let second = people.last() ?? one
        let label = if others == 0 {
            "{one.name} is here"
        } else if others == 1 {
            "{one.name} and {second.name} are here"
        } else {
            "{one.name} and {others} others are here"
        }
        ui.Button(label, faces: people.map(p -> p.name), role: .bar) {
            on press: session.toggle_collaborators()
        }
    }
}

/// An agent here, as its presence: a chip that opens its menu. One seated at the person's door
/// is stopped by ending the connection they admitted.
view agent_chip(d: Desk, a: Agent) {
    ui.MenuButton(a.name, icon: .bot, role: .bar) {
        if a.seat {
            ui.MenuItem("Stop", icon: .square, destructive: true) {
                on choose: device.stop_agent(device.desk, agent: a.key)
            }
        }
    }
}
