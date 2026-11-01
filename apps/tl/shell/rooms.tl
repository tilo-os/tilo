/// The workspace overview: every workspace the person is in, with who is there and what it
/// holds, to switch to, join, or make one. It is not the window overview (that is the desk's
/// windows and strips); a workspace is where a person and their team work, and a window is
/// where something shows.
///
/// It reads the desk's rooms (`session.room`): each one's name, who is in it, how many are
/// here now, and what happened there last. Switching, joining and making one are the
/// session's actions.
view workspaces(d: Desk) {
    lens name: Text = ""
    stack(gap: .section) {
        ui.Text("Workspaces", role: .title)
        grid(min: 18rem) {
            for r in d.rooms.sort_by(r -> r.name) {
                room_card(r)
            }
        }
        row(gap: .related, align: .end, wrap: true) {
            ui.TextInput(value: name, placeholder: "Name a new workspace", fill) {
                on submit: create_room(d, name)
            }
            ui.Button("Create", icon: .plus, role: .primary) {
                on press: create_room(d, name)
            }
            ui.Button("Join a workspace", icon: .users) {
                on press: join_workspace(d)
            }
        }
    }
}

/// One workspace: its name, who is in it, and what happened last. The one in front is lifted,
/// and pressing it does nothing; the others switch to it.
view room_card(r: Room) {
    ui.Card(item: r, title: r.name, selected: r.current) {
        stack(gap: .related) {
            ui.Text(r.activity, role: .caption, secondary: true, lines: 2)
            row(gap: .tight, align: .center, wrap: true) {
                ui.Icon(.user, size: .small, secondary: true)
                ui.Text(r.members, role: .caption)
                if r.present > 0 {
                    ui.Badge(count: r.present)
                }
            }
        }
        on activate: switch_to_key(r)
    }
}
