app so.tilo.lang.notes "Notes" (icon: .file_text, category: .office)

/// A note: a title, a body and the tags people gave it.
type Note {
    title: Text (title)
    body: Text (body)
    pinned: Bool = false
    tags: Set<Text>

    search {
        filter pinned (icon: .pin)
        filter tags
    }
}

/// The workspace's notes.
type Notes = Collection<Note>

/// The order a notebook lists its notes in.
enum NoteOrder {
    /// Date created
    created
    /// Title
    title
}

root notes: Notes

default view notebook for notes

default view note_editor for Note

row view note_row for Note

/// Starts a note. Its title starts empty: what is typed first names it.
primary action new_note(notes: Notes) -> Note (icon: .plus) {
    notes.insert(Note(title: ""))
}

/// Pins or unpins a note.
action pin(n: Note, pinned: Bool = not n.pinned) (label: if pinned { "Pin" } else { "Unpin" }, icon: .pin, on: n.pinned) {
    n.pinned = pinned
}

/// Deletes a note.
destructive action delete(notes: Notes, n: Note) (icon: .trash) {
    notes.remove(n)
}

/// Renames a note.
action retitle(n: Note, title: Text) {
    n.title = title
}

/// Gives a note a tag.
action tag(n: Note, t: Text) (icon: .tag) {
    n.tags.add(t)
}

/// Takes a tag off a note.
action untag(n: Note, t: Text) {
    n.tags.remove(t)
}

/// The notes in an order.
fn listed(notes: Notes, order: NoteOrder) -> List<Note> {
    match order {
        .created => notes,
        .title => notes.sort_by(.title),
    }
}

/// The notebook's window: its notes, each opening in a window of its own. The order is the
/// window's, so everyone looking at it sees the same list.
view notebook(notes: Notes) {
    window order: NoteOrder = .created
    std.list(notes, listed(notes, order), sort: order, empty: "No notes yet")
}

/// One note in a list: its title, the start of its body, and a pin when it is pinned.
view note_row(n: Note) {
    ui.ListRow(title: if n.title.len() == 0 { "Untitled" } else { n.title }, secondary: n.body, mark: if n.pinned { .pin })
}

/// A note's window: its title, its tags as chips that come off with their remove button, its
/// body, and what happened to it.
view note_editor(n: Note) {
    std.document(n) {
        ui.TextInput(value: n.title, placeholder: "Untitled")
        if not n.tags.is_empty() {
            row(gap: .related, wrap: true) {
                for t in n.tags {
                    ui.Chip(t) {
                        on remove: untag(n, t)
                    }
                }
            }
        }
        ui.TextInput(value: n.body, placeholder: "Write something", lines: 8)
        std.history(n)
    }
}

/// A notebook a week into a project: enough notes to scroll, a few of them pinned.
scenario "a week of notes" -> Note {
    let plan = notes.insert(Note(title: "Q3 plan", body: "Ship the offline editor, then sharing. Hiring stays flat until October.", pinned: true))
    plan.tags.add("planning")
    plan.tags.add("work")
    plan.tags.add("q3")
    notes.insert(Note(title: "Standup, Monday", body: "Ada is on the sync bug. Bo pairs with Chen on the importer."))
    notes.insert(Note(title: "Groceries", body: "Oat milk, eggs, sourdough, two lemons, coffee beans.", pinned: true))
    notes.insert(Note(title: "Interview questions", body: "Ask about the last project they were proud of, and what they would change."))
    notes.insert(Note(title: "Standup, Tuesday", body: "Sync bug reproduced on two laptops. Importer handles tables now."))
    notes.insert(Note(title: "Book list", body: "The Dispossessed, Piranesi, A Psalm for the Wild-Built."))
    notes.insert(Note(title: "Flat viewing", body: "Saturday at eleven. Bring the deposit papers and measure the bedroom."))
    notes.insert(Note(title: "Standup, Wednesday", body: "Release candidate cut. Docs still need the new screenshots."))
    notes.insert(Note(title: "Gift ideas", body: "A good kitchen knife for Sam, concert tickets for Mia."))
    notes.insert(Note(title: "Retro notes", body: "Fewer meetings on Thursdays. Keep the demo at the end of each week."))
    notes.insert(Note(title: "Standup, Thursday", body: "Two blockers left. Chen is out on Friday."))
    notes.insert(Note(title: "Running plan", body: "Three easy runs and one long run a week, up to fifteen kilometres.", pinned: true))
    notes.insert(Note(title: "Recipe: lentil soup", body: "Onion, carrot, red lentils, cumin, a tin of tomatoes, lemon at the end."))
    notes.insert(Note(title: "Standup, Friday", body: "Release went out at four. Nobody deploys over the weekend."))
    plan
}

/// A notebook just started: one note, half written.
scenario "a first note" {
    notes.insert(Note(title: "Ideas for the garden", body: "Tomatoes along the south wall, herbs by the door."))
}
