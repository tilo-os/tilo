app so.tilo.lang.gallery "Gallery" (icon: .layout_grid, category: .system)

/// The toolkit's components as an app written in tilolang places them: each `ui.*` component,
/// in the groups people meet them in, with the values an app would give it. Each group is a
/// file of its own (`controls.tl`, `charts.tl`, ...) with its own window and scenario, and a
/// test fails while a component of the toolkit is shown in none of them.

/// The everyday parts of a window: text and buttons, rows and sections, cards and facts.
type Basics {
    title: Text (title)
    note: Text
    done: Bool = false
}

root basics: Basics

default view basics for basics

/// The basics' window.
view basics(b: Basics) {
    std.page(b) {
        ui.Section(title: "Text and buttons") {
            ui.Text("A large title", role: .large_title)
            ui.Text("Body text says what a thing is, at the reading measure.")
            ui.Text("A caption under it", role: .caption, secondary: true)
            row(gap: .related) {
                ui.Button("Save", icon: .check, role: .primary)
                ui.Button("Share", icon: .share)
                ui.Button("Delete", icon: .trash, role: .destructive)
                ui.MenuButton("More", icon: .more_horizontal, icon_only: true) {
                    ui.MenuItem("Rename", icon: .pencil)
                    ui.MenuSeparator()
                    ui.MenuItem("Delete", icon: .trash, destructive: true)
                }
                ui.SearchButton()
            }
            row(gap: .related) {
                ui.Chip("Design", icon: .tag)
                ui.Avatar(b.title)
            }
        }
        ui.Section(title: "Facts") {
            ui.PropertyList {
                ui.Property("Owner", "Ada Lovelace")
                ui.Property("Due", "Friday")
            }
        }
        ui.Card(title: "A card", count: 3) {
            ui.Text("What the card summarises.")
        }
        ui.EmptyState("Nothing here yet", icon: .inbox)
        ui.TextInput(value: b.note, placeholder: "A note")
    }
}

/// The basics, filled in.
scenario "basics" -> Basics {
    basics.title = "Basics"
    basics
}
