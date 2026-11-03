app so.tilo.lang.richbody "Notebook" (icon: .file_text, category: .office)

/// A note: a title, and a body of rich text.
type Note {
    title: Text (title)
    body: RichText (body)
}

/// The notebook's notes.
type Notes = Collection<Note>

root notes: Notes

default view notebook for notes

default view note_editor for Note

row view note_row for Note

/// Starts a note. Its title starts empty: what is typed first names it.
primary action new_note(notes: Notes) -> Note (icon: .plus) {
    notes.insert(Note(title: ""))
}

/// A note's title as a row shows it.
fn name_of(n: Note) -> Text {
    if n.title.len() == 0 { "Untitled" } else { n.title }
}

/// The notebook's window: its notes, each opening in a window of its own.
view notebook(notes: Notes) {
    std.list(notes, notes, empty: "No notes yet")
}

/// One note in a list: its title, and the start of its body.
view note_row(n: Note) {
    ui.ListRow(title: name_of(n), secondary: n.body.excerpt)
}

/// A note's window: its title, its body as rich text, and what happened to it.
view note_editor(n: Note) {
    std.document(n) {
        ui.TextInput(value: n.title, placeholder: "Untitled")
        ui.BlockEditor(bind: n.body, placeholder: "Write something")
        std.history(n)
    }
}
