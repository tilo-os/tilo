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

type Tag {
    name: Text
}

root files: Shelf

default view listing for Shelf

row view note_row for Note

place view shelf_place for Shelf

default view tag_page for Tag

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

/// The shelf as a place on the canvas shows it.
view shelf_place(s: Shelf) {
    text("A shelf, placed")
}

/// A tag as the system opens one: no layout here shows a tag and no view calls this one, so it is
/// the default view that says it can be drawn.
view tag_page(t: Tag) {
    text(t.name)
}

scenario "a folder and a note" {
    let s = root Shelf
    s.items.insert(Folder(name: "Work"))
    s.items.insert(Note(name: "An idea"))
}
