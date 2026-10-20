app so.tilo.notes "Notes"

/// A note: a title, a body and the tags people gave it.
type Note {
    title: Text
    body: Text
    pinned: Bool = false
    tags: Set<Text>
}

/// A notebook holds notes and remembers which one is open.
type Notebook {
    notes: Collection<Note>
    open: Option<Note>
}

/// Starts a note and opens it.
action new_note(nb: Notebook) -> Note {
    let n = nb.notes.insert(Note(title: "Untitled"))
    nb.open = n
    return n
}

/// Opens a note in the editor.
action open_note(nb: Notebook, n: Note) {
    nb.open = n
}

/// Pins or unpins a note.
action toggle_pin(n: Note) {
    n.pinned = not n.pinned
}

/// Deletes a note; the editor closes when it was the open one.
action delete(nb: Notebook, n: Note) {
    nb.notes.remove(n)
    if nb.open.is_some() {
        if nb.open.unwrap() == n {
            nb.open = none
        }
    }
}

/// Renames a note.
action retitle(n: Note, title: Text) {
    n.title = title
}

/// Gives a note a tag.
action tag(n: Note, t: Text) {
    n.tags.add(t)
}

view main(nb: Notebook) {
    page(full) {
        split(side: leading, collapse: navigate) {
            primary(min: 16rem) {
                note_list(nb)
            }
            secondary(min: 24rem) {
                if nb.open.is_some() {
                    editor(nb, nb.open.unwrap())
                }
            }
        }
    }
}

view note_list(nb: Notebook) {
    stack {
        row(distribute: between) {
            heading("Notes")
            button("New note", on: new_note(nb)) #new
        }
        scroll {
            for n in nb.notes {
                note_row(nb, n)
            }
        }
    }
}

fn is_open(nb: Notebook, n: Note) -> Bool {
    if nb.open.is_some() {
        return nb.open.unwrap() == n
    }
    return false
}

view note_row(nb: Notebook, n: Note) {
    list_row(n.title, secondary: n.body, selected: is_open(nb, n), on: open_note(nb, n))
}

view editor(nb: Notebook, n: Note) {
    stack(gap: section) {
        row {
            input(bind: n.title, placeholder: "Title", fill)
            button(if n.pinned then "Unpin" else "Pin", on: toggle_pin(n))
            button("Delete", on: delete(nb, n)) #delete
        }
        input(bind: n.body, placeholder: "Write something", fill)
    }
}
