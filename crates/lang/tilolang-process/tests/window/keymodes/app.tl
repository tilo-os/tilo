app so.tilo.lang.keymodes "Key modes" (icon: .keyboard, category: .utility)

/// The panel whose view state a key changes.
type Panel {
    title: Text = "Shortcuts"
}

root Panel

default view panel for Panel

/// Two modes, each a button that sets the view's own state. A button's `key:` answers its press
/// while the window has the keyboard, so the mode is one key away; the third button stands behind
/// a flag that stays off, so its key answers nothing while it is not drawn.
view panel(p: Panel) {
    state mode: Text = "basic"
    state extra: Bool = false
    page {
        stack(gap: .section) {
            ui.Text(if mode == "basic" { "Basic mode" } else if mode == "scientific" { "Scientific mode" } else { "Graph mode" })
            row(gap: .related) {
                ui.Button("Basic", key: "Ctrl+1") {
                    on press: set(mode, "basic")
                }
                ui.Button("Scientific", key: "Ctrl+2") {
                    on press: set(mode, "scientific")
                }
            }
            if extra {
                ui.Button("Graph", key: "Ctrl+3") {
                    on press: set(mode, "graph")
                }
            }
        }
    }
}

scenario "two modes" {
    let p = root Panel
    p.title = "Shortcuts"
}
