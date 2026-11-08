app so.tilo.lang.notes "Notes" (icon: .file_text, category: .office)

use so.tilo.lang.discussion as discussion

/// A note: a title, a body and the tags people gave it.
type Note {
    title: Text
    body: RichText
    pinned: Bool = false
    tags: Set<Text>

    search {
        filter pinned (icon: .pin)
        filter tags
    }
}

/// The workspace's notebook: its notes, in the order it lists them.
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

root notebook: Notebook

default view notebook for Notebook

default view note_editor for Note

row view note_row for Note

/// Starts a note. Its title starts empty: what is typed first names it.
primary action new_note(nb: Notebook) -> Note (icon: .plus, group: "Note", frequent: true) {
    nb.notes.insert(Note(title: ""))
}

/// Pins or unpins a note.
action pin(n: Note, pinned: Bool = not n.pinned) (
    label: if pinned { "Pin" } else { "Unpin" },
    icon: .pin,
    on: n.pinned,
    group: "Note",
    frequent: true,
) {
    n.pinned = pinned
}

/// Deletes a note.
destructive action delete(nb: Notebook, n: Note) (icon: .trash, group: "Note") {
    nb.notes.remove(n)
}

/// Renames a note.
action retitle(n: Note, title: Text) {
    n.title = title
}

/// Gives a note a tag.
action tag(n: Note, t: Text) (icon: .tag, group: "Tags", frequent: true) {
    n.tags.add(t)
}

/// Takes a tag off a note.
action untag(n: Note, t: Text) (label: "Untag", group: "Tags") {
    n.tags.remove(t)
}

/// The notes of a notebook, in an order.
fn listed(nb: Notebook, order: NoteOrder) -> List<Note> {
    let notes: List<Note> = nb.notes
    match order {
        .created => notes,
        .title => notes.sort_by(.title),
    }
}

/// The notebook's window: its notes, each opening in a window of its own, in the order the
/// person looking chose, under the place they chose.
view notebook(nb: Notebook) {
    state order: NoteOrder = .created
    // The note this window's list shows opened where it stands.
    state opened: Option<Note> = .none
    std.browse(nb, listed(nb, order), opened, places: [Note.pinned], sort: order) { n ->
        note_row(n)
    }
}

/// One note in a list: its title, the start of its body, and a pin when it is pinned.
view note_row(n: Note) {
    ui.ListRow(
        title: if n.title.len() == 0 { "Untitled" } else { n.title },
        secondary: n.body.excerpt,
        mark: if n.pinned { .pin },
    )
}

/// A note's window: its title, its tags as chips that come off with their remove button, its
/// body, and what happened to it.
view note_editor(n: Note) {
    std.document(n) {
        ui.TextInput(value: n.title, placeholder: "Untitled", title: true)
        if not n.tags.is_empty() {
            row(gap: .related, wrap: true) {
                for t in n.tags {
                    ui.Chip(t) {
                        on remove: untag(n, t)
                    }
                }
            }
        }
        ui.BlockEditor(bind: n.body, placeholder: "Write something")
        discussion.thread(n)
        std.history(n)
    }
}

/// A notebook a week into a project: enough notes to scroll, a few of them pinned.
scenario "a week of notes" -> Note {
    let plan = notebook.notes.insert(
        Note(
            title: "Q3 plan",
            body: RichText(
                "Ship the offline editor, then sharing. Hiring stays flat until October.",
            ),
            pinned: true,
        ),
    )
    plan.tags.add("planning")
    plan.tags.add("work")
    plan.tags.add("q3")
    notebook.notes.insert(
        Note(
            title: "Standup, Monday",
            body: RichText("Ada is on the sync bug. Bo pairs with Chen on the importer."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Groceries",
            body: RichText("Oat milk, eggs, sourdough, two lemons, coffee beans."),
            pinned: true,
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Interview questions",
            body: RichText(
                "Ask about the last project they were proud of, and what they would change.",
            ),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Standup, Tuesday",
            body: RichText("Sync bug reproduced on two laptops. Importer handles tables now."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Book list",
            body: RichText("The Dispossessed, Piranesi, A Psalm for the Wild-Built."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Flat viewing",
            body: RichText(
                "Saturday at eleven. Bring the deposit papers and measure the bedroom.",
            ),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Standup, Wednesday",
            body: RichText("Release candidate cut. Docs still need the new screenshots."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Gift ideas",
            body: RichText("A good kitchen knife for Sam, concert tickets for Mia."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Retro notes",
            body: RichText("Fewer meetings on Thursdays. Keep the demo at the end of each week."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Standup, Thursday",
            body: RichText("Two blockers left. Chen is out of office."),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Running plan",
            body: RichText("Three easy runs and one long run a week, up to fifteen kilometres."),
            pinned: true,
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Recipe: lentil soup",
            body: RichText(
                "Onion, carrot, red lentils, cumin, a tin of tomatoes, lemon at the end.",
            ),
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Standup, Friday",
            body: RichText("Release went out at four. Nobody deploys over the weekend."),
        ),
    )
    plan
}

/// A notebook just started: one note, half written.
scenario "a first note" {
    notebook.notes.insert(
        Note(
            title: "Ideas for the garden",
            body: RichText("Tomatoes along the south wall, herbs by the door."),
        ),
    )
}
