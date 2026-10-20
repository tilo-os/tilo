app so.tilo.notes "Notes"

/// A note: a title, a body and the tags people gave it.
type Note {
    title: Text
    body: Text
    pinned: Bool = false
    tags: Set<Text>
}

/// The workspace's notes.
type Notebook {
    notes: Collection<Note>
}

/// The order a notebook lists its notes in.
enum NoteOrder {
    /// The order they were written in.
    created
    /// By title.
    title
}

/// Which notes a notebook lists.
enum NoteFilter {
    all
    pinned
}

root Notebook

default view notebook for Notebook

default view note_editor for Note

/// Starts a note.
action new_note(nb: Notebook) -> Note {
    nb.notes.insert(Note(title: "Untitled"))
}

/// Pins or unpins a note.
action toggle_pin(n: Note) {
    n.pinned = not n.pinned
}

/// Deletes a note.
action delete(nb: Notebook, n: Note) {
    nb.notes.remove(n)
}

/// Renames a note.
action retitle(n: Note, title: Text) {
    n.title = title
}

/// Gives a note a tag.
action tag(n: Note, t: Text) {
    n.tags.add(t)
}

/// The notes a notebook lists, in its order.
fn listed(nb: Notebook, order: NoteOrder, filter: NoteFilter) -> List<Note> {
    let notes: List<Note> = match filter {
        .all => nb.notes,
        .pinned => nb.notes.filter(n -> n.pinned),
    }
    match order {
        .created => notes,
        .title => notes.sort_by(n -> n.title),
    }
}

/// The notebook's window: its notes, each opening in a window of its own.
view notebook(nb: Notebook) {
    window order: NoteOrder = .created
    window filter: NoteFilter = .all
    lens expanded: Option<Note> = .none
    page {
        stack(gap: .section) {
            row(distribute: .between) {
                heading("Notes")
                button("New note", on: new_note(nb), style: .primary) as new
            }
            row(gap: .related) {
                button("All", on: set(filter, .all), style: if filter == .all { .primary } else { .plain }) as all
                button("Pinned", on: set(filter, .pinned), style: if filter == .pinned { .primary } else { .plain }) as pinned
                spacer
                button(if order == .title { "Sort by date" } else { "Sort by title" }, on: set(order, if order == .title { .created } else { .title })) as sort
            }
            scroll {
                stack(gap: .related) {
                    for n in listed(nb, order, filter) {
                        note_row(nb, n, expanded == .some(n), if expanded == .some(n) { set(expanded, .none) } else { set(expanded, .some(n)) })
                    }
                }
            }
        }
    }
}

/// One note in the list: its title opens it; its actions show when the row is expanded.
view note_row(nb: Notebook, n: Note, expanded: Bool, toggle: Action) {
    stack(gap: .tight) {
        row(gap: .related) {
            list_row(n.title, secondary: n.body, on: open(n), fill)
            button(if expanded { "Hide actions" } else { "Show actions" }, on: toggle, style: .plain)
        }
        if expanded {
            row(gap: .related) {
                button(if n.pinned { "Unpin" } else { "Pin" }, on: toggle_pin(n))
                button("Delete", on: delete(nb, n), style: .destructive)
            }
        }
    }
}

/// A note's window.
view note_editor(n: Note) {
    page {
        stack(gap: .section) {
            row {
                input(bind: n.title, placeholder: "Title", fill)
                button(if n.pinned { "Unpin" } else { "Pin" }, on: toggle_pin(n)) as pin
            }
            input(bind: n.body, placeholder: "Write something", lines: 8, fill)
        }
    }
}
