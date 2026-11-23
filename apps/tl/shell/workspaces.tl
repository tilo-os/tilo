/// The workspace overview: every workspace the person is in, to switch to, join, or make one.
/// It is not the window overview (that is the layout's windows and strips); a workspace is where a
/// person and their team work, and a window is where something shows.
///
/// It reads the layout's workspaces (`session.workspace`) and, for the one in front, the people
/// here. Switching, joining and making one are the session's actions.
view workspaces() {
    state name: Text = ""
    // The cards stand in the shell's page, as the overview's do: their corners round, and a card
    // on the window's edge has them cut by it.
    page(full) {
        stack(gap: .section) {
            ui.Text("Workspaces", role: .title)
            grid(min: 18rem) {
                for w in layout.workspaces.sort_by(w -> w.name) {
                    workspace_card(w)
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
/// and pressing it does nothing; the others switch to it. Where the shell is a page and the
/// workspace opens as one of its own (`LayoutWorkspace.home`), its card opens that page, as the
/// workspace switcher has always led from one workspace to another.
view workspace_card(w: LayoutWorkspace) {
    if w.home != "" {
        ui.Card(title: w.name, selected: w.current) {
            ui.Link(w.home, label: "Open")
        }
    } else {
        ui.Card(item: w, title: w.name, selected: w.current) {
            if w.current and not layout.people.is_empty() {
                ui.Facts {
                    ui.Fact("{layout.people.len()} here", icon: .users)
                }
            }
            on activate: session.switch_workspace(workspace: w.key)
        }
    }
}
