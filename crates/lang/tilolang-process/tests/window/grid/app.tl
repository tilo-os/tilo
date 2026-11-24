app so.tilo.lang.grid "Grid" (icon: .layout_grid, category: .office)

/// Objects a person moves through with the keyboard, one card each.
type Item {
    name: Text
}

/// The cards, and what the keyboard last did with them.
type Grid {
    title: Text
    items: Collection<Item>
    selected: Text
    opened: Text
    edited: Text
    removed: Text
}

root grid: Grid

/// A grid starts with the six objects a person moves through.
initial action items(g: Grid) {
    g.items.insert(Item(name: "Alpha"))
    g.items.insert(Item(name: "Bravo"))
    g.items.insert(Item(name: "Charlie"))
    g.items.insert(Item(name: "Delta"))
    g.items.insert(Item(name: "Echo"))
    g.items.insert(Item(name: "Foxtrot"))
}

/// The keyboard moved to an object.
action note_selected(g: Grid, i: Item) {
    g.selected = i.name
}

/// An object was opened.
action open_item(g: Grid, i: Item) {
    g.opened = i.name
}

/// An object was opened for editing.
action edit_item(g: Grid, i: Item) {
    g.edited = i.name
}

/// An object was thrown away.
destructive action delete_item(g: Grid, i: Item) {
    g.removed = i.name
}

default view grid_view for Grid

view grid_view(g: Grid) {
    stack(gap: .section) {
        ui.Text("Selected {g.selected}", role: .caption)
        ui.Text("Opened {g.opened}", role: .caption)
        ui.Text("Edited {g.edited}", role: .caption)
        ui.Text("Deleted {g.removed}", role: .caption)
        ui.CardGrid(name: "Items") {
            on select(x: Item): note_selected(g, x)
            on edit(x: Item): edit_item(g, x)
            on delete(x: Item): delete_item(g, x)
            for i in g.items {
                ui.CardGridCard(i, title: i.name) {
                    on activate: open_item(g, i)
                    ui.Text(i.name, role: .caption, secondary: true)
                }
            }
        }
    }
}
