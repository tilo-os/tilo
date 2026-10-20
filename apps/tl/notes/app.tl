app so.tilo.notes "Notes" (icon: .bookmark)

/// A note: a title, a body and the tags people gave it.
type Note {
    title: Text (title)
    body: Text (body)
    pinned: Bool = false
    tags: Set<Text>
}

/// The workspace's notes.
type Notebook {
    notes: Collection<Note>
}

/// The order a notebook lists its notes in.
enum NoteOrder {
    /// Date created
    created
    /// Title
    title
}

/// Which notes a notebook lists.
enum NoteFilter {
    /// All notes
    all
    /// Pinned
    pinned
}

root Notebook

default view notebook for Notebook

default view note_editor for Note

row view note_row for Note

/// Starts a note.
primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    nb.notes.insert(Note(title: "Untitled"))
}

/// Pins or unpins a note.
action toggle_pin(n: Note) (icon: .pin) {
    n.pinned = not n.pinned
}

/// Deletes a note.
destructive action delete(nb: Notebook, n: Note) (icon: .trash) {
    nb.notes.remove(n)
}

/// Renames a note.
action retitle(n: Note, title: Text) {
    n.title = title
}

/// Gives a note a tag.
action tag(n: Note, t: Text) (icon: .tag) {
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

/// The notebook's window: its notes, each opening in a window of its own. The order and the
/// filter are the window's, so everyone looking at it sees the same list.
view notebook(nb: Notebook) {
    window order: NoteOrder = .created
    window filter: NoteFilter = .all
    list(listed(nb, order, filter), sort: order, filter: filter, empty: "No notes yet")
}

/// One note in a list: its title, the start of its body, and a pin when it is pinned.
view note_row(n: Note) {
    row(n.title, secondary: n.body, mark: if n.pinned { .pin })
}

/// A note's window.
view note_editor(n: Note) {
    document {
        input(bind: n.title, placeholder: "Title")
        input(bind: n.body, placeholder: "Write something", lines: 8)
    }
}

/// The pinned notes' titles, for showing inside another app's view.
view summary(nb: Notebook) {
    section("Notes") {
        stack(gap: .related) {
            for n in listed(nb, .created, .pinned) {
                button(n.title, on: open(n))
            }
        }
    }
}

/// A notebook a week into a project: enough notes to scroll, a few of them pinned.
scenario "a week of notes" {
    let nb = root Notebook
    nb.notes.insert(Note(title: "Q3 plan", body: "Ship the offline editor, then sharing. Hiring stays flat until October.", pinned: true))
    nb.notes.insert(Note(title: "Standup, Monday", body: "Ada is on the sync bug. Bo pairs with Chen on the importer."))
    nb.notes.insert(Note(title: "Groceries", body: "Oat milk, eggs, sourdough, two lemons, coffee beans.", pinned: true))
    nb.notes.insert(Note(title: "Interview questions", body: "Ask about the last project they were proud of, and what they would change."))
    nb.notes.insert(Note(title: "Standup, Tuesday", body: "Sync bug reproduced on two laptops. Importer handles tables now."))
    nb.notes.insert(Note(title: "Book list", body: "The Dispossessed, Piranesi, A Psalm for the Wild-Built."))
    nb.notes.insert(Note(title: "Flat viewing", body: "Saturday at eleven. Bring the deposit papers and measure the bedroom."))
    nb.notes.insert(Note(title: "Standup, Wednesday", body: "Release candidate cut. Docs still need the new screenshots."))
    nb.notes.insert(Note(title: "Gift ideas", body: "A good kitchen knife for Sam, concert tickets for Mia."))
    nb.notes.insert(Note(title: "Retro notes", body: "Fewer meetings on Thursdays. Keep the demo at the end of each week."))
    nb.notes.insert(Note(title: "Standup, Thursday", body: "Two blockers left. Chen is out on Friday."))
    nb.notes.insert(Note(title: "Running plan", body: "Three easy runs and one long run a week, up to fifteen kilometres.", pinned: true))
    nb.notes.insert(Note(title: "Recipe: lentil soup", body: "Onion, carrot, red lentils, cumin, a tin of tomatoes, lemon at the end."))
    nb.notes.insert(Note(title: "Standup, Friday", body: "Release went out at four. Nobody deploys over the weekend."))
}

/// A notebook just started: one note, half written.
scenario "a first note" {
    let nb = root Notebook
    nb.notes.insert(Note(title: "Ideas for the garden", body: "Tomatoes along the south wall, herbs by the door."))
}
