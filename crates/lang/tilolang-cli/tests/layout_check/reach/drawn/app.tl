app so.tilo.lang.layoutdrawn "Drawn" (icon: .layout_grid, category: .utility)

/// A shelf of folders and loose notes: one collection of a union, whose items are of either type.
type Shelf {
    items: Collection<Folder | Note>
}

type Folder {
    name: Text
}

type Note {
    name: Text
}

root files: Shelf

default view listing for Shelf

row view note_row for Note

/// The shelf's own window.
view listing(s: Shelf) {
    text("A shelf")
}

/// The row another app's page draws, and the helper only a shelf with nothing in it reaches.
view line(s: Shelf) {
    if s.items.is_empty() {
        behind(s)
    } else {
        text("Drawn from another app")
    }
}

/// A view only a shelf with nothing in it draws: no scenario here stages an empty one.
view behind(s: Shelf) {
    text("Nothing stages this")
}

/// A view nothing calls.
view orphan(s: Shelf) {
    text("Nothing calls this")
}

/// A note as the system shows one, in a search result or another app's page.
view note_row(n: Note) {
    row(n.name)
}

scenario "a folder and a note" {
    let s = root Shelf
    s.items.insert(Folder(name: "Work"))
    s.items.insert(Note(name: "An idea"))
}
