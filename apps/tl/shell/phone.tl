/// The shell on a phone: the Tilo app on a device that arranges nothing, so the shell is one page
/// at a time. A person signs in to what their other devices already have, or starts as a new
/// person when this is their first device (`device.found`), chooses one of their
/// workspaces, and opens an app from it; Back returns to the workspaces.
///
/// It reads the phone's own device (`device.devices`, `device.apps`) and no layout: the workspace
/// the person picked is this view's state, never a document, so nothing another device shows
/// depends on it. Picking one switches the session's workspace as any other surface does
/// (`session.switch_workspace`), so the apps the page lists are that workspace's; a press on one is
/// the session's `session.open_app`, which the phone's host carries out, so an app entered here is
/// entered the one way every surface enters one.
view phone() {
    state picked: Text = ""
    state ticket: Text = ""
    let mine = device.devices
    page(full) {
        stack(gap: .section) {
            if not mine.signed_in {
                ui.Text("Sign in to Tilo", role: .title)
                ui.Text(
                    "On a device that has your identity, run tilo device add and type the code it prints here. If this is your first device, start new.",
                    role: .caption,
                    secondary: true,
                )
                if mine.refused != "" {
                    ui.Banner("Not signed in", message: mine.refused, kind: .error)
                }
                ui.TextInput(bind: ticket, placeholder: "tilo1…") {
                    on submit: device.log_in(device.devices, ticket: ticket)
                }
                row(gap: .related, align: .center) {
                    ui.Button("Sign in", icon: .log_in, role: .primary) {
                        on press: device.log_in(device.devices, ticket: ticket)
                    }
                    ui.Button("Start new", icon: .plus, role: .plain) {
                        on press: device.found(device.devices)
                    }
                }
            } else if picked == "" {
                ui.Text("Workspaces", role: .title)
                if mine.workspaces.is_empty() {
                    ui.EmptyState("No workspaces here yet", icon: .layers, inline: true)
                }
                for w in mine.workspaces.sort_by(w -> w.name) {
                    ui.ListRow(w.name, icon: .layers) {
                        on activate: session
                            .switch_workspace(workspace: w.key)
                            .then(set(picked, w.key))
                    }
                }
            } else {
                row(gap: .related, align: .center) {
                    ui.Button("Back", icon: .arrow_left, role: .plain, size: .small) {
                        on press: set(picked, "")
                    }
                    if let w = mine.workspaces.find(w -> w.key == picked) {
                        ui.Text(w.name, role: .title)
                    }
                    spacer
                }
                ui.Section("Apps") {
                    if device.apps.installed.is_empty() {
                        ui.EmptyState("No apps yet", icon: .app_window, inline: true)
                    }
                    for a in device.apps.installed.sort_by(a -> a.name) {
                        app_row(a)
                    }
                }
            }
        }
    }
}
