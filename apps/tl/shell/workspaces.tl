/// The workspace overview: every workspace the person is in, to switch to, join, or make one.
/// It is not the window overview (that is the desk's windows and strips); a workspace is where a
/// person and their team work, and a window is where something shows.
///
/// It reads the desk's workspaces (`session.workspace`) and, for the one in front, the people
/// here. Switching, joining and making one are the session's actions.
view workspaces(d: Desk) {
    state name: Text = ""
    // The cards stand in the desk's page, as the overview's do: their corners round, and a card
    // on the window's edge has them cut by it.
    page(full) {
        stack(gap: .section) {
            ui.Text("Workspaces", role: .title)
            grid(min: 18rem) {
                for w in d.workspaces.sort_by(w -> w.name) {
                    workspace_card(d, w)
                }
            }
            row(gap: .related, align: .end, wrap: true) {
                ui.TextInput(bind: name, placeholder: "Name a new workspace", fill) {
                    on submit: session.new_workspace(name: name).then(set(name, ""))
                }
                ui.Button("Create", icon: .plus, role: .primary) {
                    on press: session.new_workspace(name: name)
                }
                ui.Button("Join a workspace", icon: .users) {
                    on press: session.join_workspace()
                }
            }
        }
    }
}

/// One workspace: its name, and for the one in front, who is here. The one in front is lifted,
/// and pressing it does nothing; the others switch to it.
view workspace_card(d: Desk, w: Workspace) {
    ui.Card(item: w, title: w.name, selected: w.current) {
        if w.current and not d.people.is_empty() {
            ui.Facts {
                ui.Fact("{d.people.len()} here", icon: .users)
            }
        }
        on activate: device.switch_workspace(device.desk, workspace: w.key)
    }
}
