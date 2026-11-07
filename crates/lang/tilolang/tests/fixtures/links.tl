app so.tilo.links "Links"

/// A note: what it says.
type Note {
    title: Text
    body: RichText
}

/// An item about a note, when it is about one.
type Item {
    title: Text
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
    return linked(n).map(l -> "{l.how.name()} {meta(l.of).title}")
}

/// What names the note in a field, by its title.
action naming(n: Note) -> List<Text> {
    return linked(n).filter(l -> l.how == .field).map(l -> meta(l.of).title)
}

/// What mentions the note in its text, by its title.
action mentioning(n: Note) -> List<Text> {
    return linked(n).filter(l -> l.how == .mention).map(l -> meta(l.of).title)
}

/// The board: its notes.
view board(b: Board) {
    ui.NavShell(content_kind: .list) {
        content: {
            if b.notes.is_empty() {
                ui.EmptyState("Nothing yet", icon: .file_text)
            } else {
                std.list(b.notes) { (n, at) ->
                    ui.ListItem(item: n, presence: at) {
                        note_row(n)
                    }
                }
            }
        }
    }
}

/// A note in a list: its title.
view note_row(n: Note) {
    row(n.title)
}

/// An item in a list: its title.
view item_row(i: Item) {
    row(i.title)
}

/// The note the board holds under `title`, when it holds one.
action note_titled(b: Board, title: Text) -> Option<Note> {
    return b.notes.find(n -> n.title == title)
}

/// A note made from another: the scenario makes it from the one it opens on, as the menu's Make
/// does for a person, and answers the note it made.
scenario "a note made from another" -> Note {
    let b = root Board
    let a = add_note(b, "A")
    invoke(add_note(b, "B"), from: a)
}

/// Writes a note under the one the page shows.
action write_under(n: Note, title: Text = "under") -> Note (label: "Write") {
    let b = root Board
    b.notes.insert(Note(title: title, body: RichText("")))
}

/// A note's page: what is written under it is written from it, so the button that writes says
/// `.made_from(n)` and the change it makes records the note as what it was made from.
view note_page(n: Note) {
    state draft: Text
    stack {
        input(bind: draft, placeholder: "Write")
        button(write_under(n).made_from(n))
    }
}

/// What links to a note: how it is linked, and the linked resource's title.
view links(n: Note) {
    stack {
        for l in linked(n) {
            text("{l.how.name()} {meta(l.of).title}")
        }
    }
}
