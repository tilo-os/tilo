app so.tilo.lang.notes "Notes" (icon: .file_text, category: .office)

use so.tilo.lang.discussion as discussion

/// A note: a title, a body, the note it stands under and the tags people gave it.
type Note (untitled: "Untitled") {
    title: Text
    body: RichText
    parent: Option<Note>
    pinned: Bool = false
    tags: Set<Text>
    /// The file the note was imported from, by the address of its content: importing the same
    /// file again opens this note rather than making a second one.
    source: Text = ""
    /// The name that file had, path inside its archive included: an import that keeps a
    /// package's nesting finds the note a page stands under by it.
    origin: Text = ""

    search {
        filter pinned (icon: .pin)
        filter tags
    }
}

/// The workspace's notebook: its notes, in the tree's order.
type Notebook {
    notes: Collection<Note>
}

root notebook: Notebook

default view notebook for Notebook

default view note_editor for Note

row view note_row for Note

/// Starts a note at the top of the tree. Its title starts empty: what is typed first names it.
primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    nb.notes.insert(Note(title: ""))
}

/// Starts a note inside `n`'s own, after the notes it already holds.
action new_child(nb: Notebook, n: Note) -> Note (icon: .plus, label: "New sub-note") {
    let made = nb.notes.insert(Note(title: "", parent: n))
    if let last = nb.notes.filter(x -> x.parent == n and x != made).last() {
        nb.notes.move(made, after: last)
    } else { nb.notes.move(made, after: n) }
    made
}

/// Pins or unpins a note.
action pin(n: Note, pinned: Bool = not n.pinned) (
    label: if pinned { "Pin" } else { "Unpin" },
    icon: .pin,
    on: n.pinned,
) {
    n.pinned = pinned
}

/// Moves a note to a place in the tree: at the end of `parent`'s own, or where `before` stands.
action move_to(nb: Notebook, moved: Note, parent: Option<Note>, before: Option<Note>) (
    label: "Move to",
) {
    require not within(nb.notes, parent, moved) and not within(nb.notes, before?.parent, moved) "A note cannot go inside itself"
    if let b = before {
        moved.parent = b.parent
        nb.notes.move(moved, before: b)
    } else {
        moved.parent = parent
        if let last = nb.notes.filter(x -> x.parent == parent and x != moved).last() {
            nb.notes.move(moved, after: last)
        } else if let p = parent { nb.notes.move(moved, after: p) }
    }
}

/// Copies a note, with every note under it, just after the one it copies.
action duplicate(nb: Notebook, n: Note) -> Note (
    icon: .copy,
    label: "Duplicate",
    key: "Ctrl+Shift+D",
) {
    let under = nb.notes.descendants(n, .parent)
    let copy = nb.notes.insert(copied(n, n.parent))
    for t in n.tags {
        copy.tags.add(t)
    }
    if let last = last_below(nb.notes, n) {
        nb.notes.move(copy, after: last)
    } else { nb.notes.move(copy, after: n) }
    // Each copy hangs under the copy of the note it stands under, at the place it takes itself.
    for x in under {
        let under_copy = under.index_of(x.parent ?? n) ?? -1
        let at = nb.notes.descendants(copy, .parent).at(under_copy) ?? copy
        let below = last_below(nb.notes, at) ?? at
        let made = nb.notes.insert(copied(x, at))
        for t in x.tags {
            made.tags.add(t)
        }
        nb.notes.move(made, after: below)
    }
    copy
}

/// Puts a note's own in the order of their titles, once, for a branch that grew out of order.
action sort_children(nb: Notebook, n: Note) (icon: .a_arrow_down, label: "Sort by title") {
    applies not nb.notes.filter(x -> x.parent == n).is_empty() "It holds no notes"
    let kids = nb.notes.filter(x -> x.parent == n).sort_by(x -> x.title)
    for k in kids {
        let at = kids.index_of(k) ?? 0
        // What it stands after: the note it is of for the first, else the one before it.
        let over = if at == 0 { n } else { kids.at(at - 1) ?? n }
        nb.notes.move(k, after: over)
    }
}

/// Deletes a note and every note under it, as one change a person takes back at once.
destructive action delete(nb: Notebook, n: Note) (icon: .trash, key: "Delete") {
    for x in nb.notes.descendants(n, .parent) {
        nb.notes.remove(x)
    }
    nb.notes.remove(n)
}

/// Renames a note.
action retitle(n: Note, title: Text) (icon: .pencil, label: "Rename") {
    n.title = title
}

/// Gives a note a tag.
action tag(n: Note, t: Text) (icon: .tag, group: "Tags") {
    n.tags.add(t)
}

/// Untags a note.
action untag(n: Note, t: Text) (label: "Untag", group: "Tags") {
    n.tags.remove(t)
}

/// Puts a picture in a note, where its words end: the person picks the file, and the note
/// keeps the address of its content rather than the picture.
action insert_image(n: Note, image: File) (icon: .image, did: "added an image") {
    n.body = RichText(n.body, "\n![]({image.url})")
}

/// A note as Markdown: its title, then its body. What `file.save` writes as the file the
/// person names. Its pictures stand as the address of their bytes, which every device of the
/// workspace holds.
action export_markdown(n: Note) -> Text (icon: .download) {
    let title = n.title.trim()
    let body = n.body.markdown.trim()
    if body.starts_with("# {title}") or title == "" { body } else { "# {title}\n\n{body}" }
}

/// The notebook as one Markdown file, in the tree's order: each note's title a heading one level
/// under the note it stands in.
action export_notebook(nb: Notebook) -> Text (icon: .download) {
    let out = ""
    for n in nb.notes {
        out = "{out}{heading_of(nb, n)} {n.title}\n\n{n.body.markdown.trim()}\n\n"
    }
    out.trim()
}

/// Imports a Markdown file as a note at the top of the tree, or opens the note that file already
/// made: a note keeps the address of the file it came from, so importing the same file twice
/// makes one note. Its title is the document's first heading, or the file's name; the heading it
/// says the title with is not said again in its body.
action import_markdown(nb: Notebook, file: File) -> Note (
    icon: .file_plus,
    did: "imported a note",
) {
    if let imported = nb.notes.find(n -> n.source == file.url) {
        return imported
    }
    let named = title_words(file.name)
    match file.text() {
        .ok(text) => {
            let title = first_heading(text) ?? named
            nb.notes.insert(
                Note(
                    title: title,
                    body: RichText.from_markdown(without_heading(text, title)),
                    source: file.url,
                    origin: file.name,
                ),
            )
        }
        // A file that is not text still makes a note, saying what became of it: the person sees
        // the file they chose, and why it is not a note's words.
        .err(why) => {
            nb.notes.insert(
                Note(title: named, body: RichText(why), source: file.url, origin: file.name),
            )
        }
    }
}

/// Imports a Notion export — the zip of a page's Markdown and its pictures — as notes under the
/// notebook, a page under the page that held it, in one change: undo takes the whole import
/// back. Each page's title is its first heading, or its file's name; the pictures a page shows
/// are the ones the export kept beside it, found among the archive's files. A page that is
/// imported already keeps the note it made.
action import_notion(nb: Notebook, file: File) (icon: .file_plus, did: "imported a notebook") {
    match file.files() {
        .ok(files) => {
            let pictures = files.filter(f -> f.media.starts_with("image/"))
            // A page under the page it stood under comes after it: its parent is a note then.
            let pages = files
                .filter(f -> f.media == "text/markdown")
                .sort_by(p -> p.name.split("/").len())
            for page in pages {
                let named = title_words(page.name)
                if nb.notes.find(n -> n.source == page.url) == .none {
                    let parent = nb.notes.find(n -> n.origin == "{folder_of(page.name)}.md")
                    let at = match page.text() {
                        .ok(text) => {
                            let title = first_heading(text) ?? named
                            let shown = pictures_shown(without_heading(text, title), pictures)
                            Note(
                                title: title,
                                body: RichText.from_markdown(shown),
                                parent: parent,
                                source: page.url,
                                origin: page.name,
                            )
                        }
                        .err(why) => {
                            Note(
                                title: named,
                                body: RichText(why),
                                parent: parent,
                                source: page.url,
                                origin: page.name,
                            )
                        }
                    }
                    let made = nb.notes.insert(at)
                    if let p = parent {
                        if let last = nb.notes.filter(x -> x.parent == p and x != made).last() {
                            nb.notes.move(made, after: last)
                        } else { nb.notes.move(made, after: p) }
                    }
                }
            }
        }
        // An archive that does not read says why, in the note it would have filled.
        .err(why) => {
            if nb.notes.find(n -> n.source == file.url) == .none {
                nb.notes.insert(
                    Note(
                        title: title_words(file.name),
                        body: RichText(why),
                        source: file.url,
                        origin: file.name,
                    ),
                )
            }
        }
    }
}

/// The notebook's window: the notes as a tree in the sidebar, the one chosen beside it.
view notebook(nb: Notebook) {
    std.tree(
        nb,
        nb.notes,
        parent: n -> n.parent,
        label: n -> meta(n).title,
        place: (moved, parent, before) -> move_to(nb, moved, parent, before),
        add: add_child,
        path: n -> trail(nb.notes, n),
        above: [Note.pinned],
        doc: note_page,
    ) { n ->
        // A note with no title yet is called what its type calls it, as in every list of it.
        note_node(n)
        head: {
            row(gap: .related) {
                button(new_note(nb))
                notebook_menu(nb)
            }
        }
    }
}

/// The notebook's own file commands, beside the control that makes a note: a Markdown file or a
/// Notion export in, the whole notebook out.
view notebook_menu(nb: Notebook) {
    ui.MenuButton("File", icon: .file_plus) {
        ui.MenuItem("Import Markdown…", icon: .file_plus) {
            on choose: file.choose(import_markdown(nb), accept: ["text/markdown"])
        }
        ui.MenuItem("Import a Notion export…", icon: .file_plus) {
            on choose: file.choose(import_notion(nb), accept: ["application/zip"])
        }
        ui.MenuItem("Export the notebook…", icon: .download) {
            on choose: file.save(export_notebook(nb), name: "Notes.md")
        }
    }
}

/// The control a row shows while it is hovered: a note inside it.
view add_child(n: Note) {
    ui.Button("New sub-note", icon: .plus, icon_only: true) {
        on press: new_child(notebook, n)
    }
}

/// A note's row in the tree.
view note_node(n: Note) {
    ui.TreeViewRow(icon: .file_text, tone: .secondary)
}

/// One note in a list: its title, the start of its body, and a pin when it is pinned.
view note_row(n: Note) {
    ui.ListRow(title: meta(n).title, secondary: n.body.excerpt, mark: if n.pinned { .pin })
}

/// A note's window of its own: its page, as a document the window is for.
view note_editor(n: Note) {
    std.document(n) {
        note_page(n)
    }
}

/// The open note: its title, its tags as chips, its body, what links to it, its discussion and
/// its history. Its body's `[[` writes a link to another of the notebook's notes.
view note_page(n: Note) {
    let nb = root Notebook
    ui.TextInput(bind: n.title, title: true)
    if not n.tags.is_empty() {
        row(gap: .related, wrap: true) {
            for t in n.tags {
                ui.Chip(t) {
                    on remove: untag(n, t)
                }
            }
        }
    }
    row(distribute: .end) {
        note_menu(n)
    }
    ui.BlockEditor(bind: n.body, placeholder: "Write something", links: nb.notes)
    std.linked(n, compact: true)
    discussion.thread(n)
    std.history(n)
}

/// A note's own file commands: a picture in, the note out as a Markdown file.
view note_menu(n: Note) {
    ui.MenuButton("File", icon: .file_plus) {
        ui.MenuItem("Insert an image…", icon: .image) {
            on choose: file.choose(
                insert_image(n),
                accept: ["image/png", "image/jpeg", "image/webp"],
            )
        }
        ui.MenuItem("Export as Markdown…", icon: .download) {
            on choose: file.save(export_markdown(n), name: "{file_name(n)}.md")
        }
    }
}

/// The heading a note's title takes in the notebook's Markdown: one level under the note it
/// stands in, at most six deep.
fn heading_of(nb: Notebook, n: Note) -> Text {
    let marks = if let p = n.parent { "{heading_of(nb, p)}#" } else { "#" }
    if marks.len() > 6 { marks.slice(0, 6) } else { marks }
}

fn within(items: List<Note>, place: Option<Note>, top: Note) -> Bool {
    if let p = place {
        p == top or items.ancestors(p, .parent).any(a -> a == top)
    } else { false }
}

/// `n` as a note of its own under `parent`: what a copy of it says, without the notes under it.
fn copied(n: Note, parent: Option<Note>) -> Note {
    Note(title: n.title, body: n.body, parent: parent, pinned: n.pinned)
}

/// The name a note is offered to be saved as: its title, a dash for each character a file name
/// cannot keep, and "Note" while it has no title.
fn file_name(n: Note) -> Text {
    let safe = n.title.trim().replace("/", "-")
    if safe == "" { "Note" } else { safe }
}

/// The name at the end of a path: a file's own name, not the folders it stands in.
fn base_name(path: Text) -> Text {
    path.split("/").last() ?? path
}

/// The folder a path stands in (`Notes/Meeting.md` is `Notes`), and nothing at the top.
fn folder_of(path: Text) -> Text {
    let parts = path.split("/")
    if parts.len() <= 1 { "" } else { parts.take(parts.len() - 1).join("/") }
}

/// The words a file's own name gives a note: the name at the end of its path, without the
/// extension (`Notes/Meeting.md` is `Meeting`).
fn title_words(name: Text) -> Text {
    let base = base_name(name)
    let dot = base.find(".") ?? base.len()
    if dot == 0 { base } else { base.slice(0, dot) }
}

/// The words of a Markdown document's first heading, when it has one.
fn first_heading(markdown: Text) -> Option<Text> {
    for line in markdown.split("\n") {
        let trimmed = line.trim()
        if trimmed.starts_with("# ") {
            let title = trimmed.slice(2, trimmed.len()).trim()
            if not title.is_empty() {
                return title
            }
        }
    }
    .none
}

/// `markdown` without the heading its title already says: a note's title is its own, never a
/// first line of its body again.
fn without_heading(markdown: Text, title: Text) -> Text {
    let heading = "# {title}\n"
    if markdown.starts_with(heading) {
        return markdown.slice(heading.len(), markdown.len()).trim()
    }
    markdown.trim()
}

/// `markdown` with each picture it shows found among `pictures`, the files an export kept beside
/// the page: a name the export wrote becomes the address of the file, which every device holds.
fn pictures_shown(markdown: Text, pictures: List<File>) -> Text {
    let shown = markdown
    for picture in pictures {
        shown = shown.replace("]({base_name(picture.name)})", "]({picture.url})")
        shown = shown.replace("]({picture.name})", "]({picture.url})")
    }
    shown
}

/// The note that ends the run `t` begins: the last note under it, or `t` itself.
fn last_below(items: List<Note>, t: Note) -> Option<Note> {
    items.filter(x -> x == t or items.ancestors(x, .parent).any(a -> a == t)).last()
}

/// Where `n` stands, as the bar over its page names it: the notes it is under, root first.
fn trail(items: List<Note>, n: Note) -> Text {
    items
        .take(items.index_of(n) ?? 0)
        .filter(a -> items.ancestors(n, .parent).contains(a))
        .map(a -> a.title)
        .join(" / ")
}

/// A notebook a week into a project: two branches of notes, one of them three deep, a few of
/// them pinned.
scenario "a week of notes" {
    let plan = notebook.notes.insert(
        Note(
            title: "Q3 plan",
            body: RichText("Ship the offline editor, then sharing."),
            pinned: true,
        ),
    )
    plan.tags.add("planning")
    let standups = notebook.notes.insert(
        Note(title: "Standups", body: RichText("Ten minutes, every morning."), parent: plan),
    )
    notebook.notes.insert(
        Note(title: "Monday", body: RichText("Ada is on the sync bug."), parent: standups),
    )
    notebook.notes.insert(
        Note(
            title: "Tuesday",
            body: RichText("The importer handles tables now."),
            parent: standups,
        ),
    )
    notebook.notes.insert(
        Note(title: "Interviews", body: RichText("Two rounds, a pairing each."), parent: plan),
    )
    let home = notebook.notes.insert(
        Note(title: "Home", body: RichText("What the flat needs, and what we cook in it.")),
    )
    notebook.notes.insert(
        Note(title: "Groceries", body: RichText("Oat milk, eggs, two lemons."), parent: home),
    )
    notebook.notes.insert(
        Note(
            title: "Lentil soup",
            body: RichText("Onion, carrot, red lentils, cumin, lemon at the end."),
            parent: home,
            pinned: true,
        ),
    )
}

/// A notebook just started: one note, half written.
scenario "a first note" -> Note {
    notebook.notes.insert(
        Note(
            title: "Ideas for the garden",
            body: RichText("Tomatoes along the south wall, herbs by the door."),
        ),
    )
}
