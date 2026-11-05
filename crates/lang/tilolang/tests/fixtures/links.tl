app so.tilo.links "Links"

/// A note: what it says.
type Note {
    title: Text (title)
    body: RichText (body)
}

/// An item about a note, when it is about one.
type Item {
    title: Text (title)
    about: Option<Note> = .none
}

/// The board: its notes and items.
type Board {
    notes: Collection<Note>
    items: Collection<Item>
}

root Board

default view board for Board

row view note_row for Note

row view item_row for Item

/// Adds a note.
action add_note(b: Board, title: Text) -> Note {
    b.notes.insert(Note(title: title, body: RichText("")))
}

/// Notes something about an existing note.
action note_about(b: Board, n: Note, title: Text) -> Note {
    b.notes.insert(Note(title: title, body: RichText("see ", n)))
}

/// Adds an item.
action add_item(b: Board, title: Text) -> Item {
    b.items.insert(Item(title: title))
}

/// Adds an item about a note.
action item_about(b: Board, n: Note, title: Text) -> Item {
    b.items.insert(Item(title: title, about: .some(n)))
}

/// Makes an item about nothing.
action clear_about(i: Item) {
    i.about = .none
}

/// Changes what a note says.
action rewrite(n: Note, body: RichText) {
    n.body = body
}

/// Takes a note off the board.
action drop_note(b: Board, n: Note) {
    b.notes.remove(n)
}

/// The title of what a note was made from; empty for one made on its own.
action made_from(n: Note) -> Text {
    if let c = created(n), f = c.from {
        return meta(f).title
    }
    return ""
}

/// What links to a note, each as how it is linked and the linked resource's title.
action links_of(n: Note) -> List<Text> {
    return linked(n).map(l -> "{l.how} {meta(l.of).title}")
}

/// The board: its notes.
view board(b: Board) {
    std.list(b, b.notes, empty: "Nothing yet")
}

/// A note in a list: its title.
view note_row(n: Note) {
    row(n.title)
}

/// An item in a list: its title.
view item_row(i: Item) {
    row(i.title)
}

/// What links to a note: how it is linked, and the linked resource's title.
view links(n: Note) {
    stack {
        for l in linked(n) {
            text("{l.how} {meta(l.of).title}")
        }
    }
}
