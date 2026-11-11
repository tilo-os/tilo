app so.tilo.lang.notes "Notes" (icon: .file_text, category: .office)

use so.tilo.lang.discussion as discussion

/// A note: a title, a body, the note it stands under and the tags people gave it.
type Note (untitled: "Untitled") {
    title: Text
    body: RichText
    /// The pictures the note shows, by the address of their content: the name each was taken in
    /// as. The body says where a picture is and no more, and an export writes it beside the note
    /// under the name it came in as, which only this says.
    images: Map<Text>
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

/// Moves a note to just after `n` and every note under it: the place a drop after a row asks
/// for, which names the row it lands after rather than the row after that one.
action move_after(nb: Notebook, moved: Note, n: Note) (label: "Move to") {
    require not within(nb.notes, n, moved) "A note cannot go inside itself"
    moved.parent = n.parent
    if let last = last_below(nb.notes, n) {
        nb.notes.move(moved, after: last)
    } else { nb.notes.move(moved, after: n) }
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
/// keeps the address of its content rather than the picture, and the name it came in as.
action insert_image(n: Note, image: File) (icon: .image, did: "added an image") {
    n.body = RichText(n.body, "\n![]({image.url})")
    n.images.put(image.url, base_name(image.name))
}

/// A note as Markdown: its title, then its body, each picture it shows standing beside it as
/// `<note>/images/<name>`, a path another reader of the file follows and this app reads again
/// as the address of the picture's bytes.
action export_markdown(n: Note) -> Text (icon: .download) {
    note_markdown(n, file_name(n))
}

/// A note as a file: its Markdown, and the pictures it shows beside it. A note that shows a
/// picture is one zip — `Q3 plan.md` and `Q3 plan/images/fox.png` — because Markdown reaches a
/// picture by a path and by nothing else; a note of words alone is that Markdown itself.
action export_note(n: Note) -> File (icon: .download) {
    if n.images.keys().is_empty() {
        File.of_text("{file_name(n)}.md", export_markdown(n))
    } else {
        File.zip("{file_name(n)}.zip", note_files(n, file_name(n), ""))
    }
}

/// The notebook as a zip, one Markdown file for each note: a note's own under the folder of the
/// note it stands in, and the pictures it shows in its own `images` folder beside it. That is the
/// shape the import reads, so the notebook comes back as the tree it is, titles, bodies and
/// pictures alike.
action export_notebook(nb: Notebook) -> File (icon: .download) {
    File.zip("Notes.zip", files_under(nb, .none, ""))
}

/// The files of the notes under `parent`, each at `folder`: a note's own, then — at the folder
/// its own name adds — the files of the notes under it. The tree is walked once from the notes
/// that stand at the top, so a notebook of ten thousand notes costs its notes, and not a walk of
/// the whole notebook for each of them.
fn files_under(nb: Notebook, parent: Option<Note>, folder: Text) -> List<File> {
    // The notes under it, in the order the notebook keeps them, and the name each takes there.
    let kids = kids_of(nb, parent)
    let names = kids.map(x -> file_name(x))
    kids.flat_map(kid -> files_of(nb, kids, names, folder, kid))
}

/// The notes that stand under `parent`, in the order the notebook keeps them: asked of the
/// store's index of the parent each note names, never by walking the notes beside them.
fn kids_of(nb: Notebook, parent: Option<Note>) -> List<Note> {
    if let p = parent {
        nb.notes.descendants(p, .parent).filter(x -> x.parent == parent)
    } else { nb.notes.filter(x -> x.parent == parent) }
}

/// The files one note of the notebook's export holds at `folder`: its own Markdown and pictures,
/// then the notes under it, at the folder the name it takes among `kids` adds.
fn files_of(
    nb: Notebook,
    kids: List<Note>,
    names: List<Text>,
    folder: Text,
    n: Note,
) -> List<File> {
    let name = name_among(kids, names, n)
    let under = "{folder}{name}"
    [note_files(n, name, folder), files_under(nb, .some(n), "{under}/")].flat_map(files -> files)
}

/// The name a note's file takes among the notes of `kids` it stands with: its title's file name,
/// and — while a note of that name stands before it under the same note — its place among them
/// (`Monday`, `Monday (2)`), so two notes of one title are two files.
fn name_among(kids: List<Note>, names: List<Text>, n: Note) -> Text {
    let name = file_name(n)
    // The note of a name no other takes beside it keeps it, as every note of a usual notebook
    // does: what it costs is one walk of the names, never a place among them.
    if (kids.at(names.index_of(name) ?? 0) ?? n) == n {
        return name
    }
    let at = kids.index_of(n) ?? 0
    "{name} ({names.take(at).filter(x -> x == name).len() + 1})"
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

/// Imports a zip of Markdown (a notebook this app exported, or a Notion export) as notes under
/// the notebook, a page under the page that held it, in one change: undo takes the whole import
/// back. Each page's title is its first heading, or its file's name; the pictures a page shows
/// are the files it stands beside, a reference read from the page's own folder. A page that is
/// imported already keeps the note it made.
action import_archive(nb: Notebook, file: File) (icon: .file_plus, did: "imported a notebook") {
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
                            let shown = pictures_shown(
                                without_heading(text, title),
                                page.name,
                                pictures,
                            )
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
                    // The pictures the page shows are the note's own, by the name each stood
                    // under: what an export of it writes beside it again.
                    let shown = made.body.markdown
                    for picture in pictures {
                        if shown.contains(picture.url) {
                            made.images.put(picture.url, base_name(picture.name))
                        }
                    }
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
    // The note the window shows, kept by the window: the first note until a person chooses
    // another, and the one a link, a breadcrumb or a search shows.
    state open_note: Option<Note> = nb.notes.first()
    on show(x: Note): set(open_note, x)
    std.tree(
        nb,
        nb.notes,
        opened: open_note,
        parent: n -> n.parent,
        depth: n -> .some(depth_of(n)),
        branch: n -> holds_any(nb, n),
        label: n -> meta(n).title,
        place: (moved, parent, before) -> move_to(nb, moved, parent, before),
        place_after: (moved, x) -> move_after(nb, moved, x),
        add: add_child,
        path: n -> meta(n).title,
        doc: note_page,
        head: notebook_head,
    ) { n ->
        // A note with no title yet is called what its type calls it, as in every list of it.
        note_node(n)
    }
}

/// The notebook's controls in the window's bar, given the note open beside the tree: its file
/// commands, the notebook's own and the open note's. What makes a note is the window's primary
/// action, which the bar offers once, so no button of the head's draws it again.
view notebook_head(open: Option<Note>) {
    notebook_menu(root Notebook, open)
}

/// The notebook's file commands: a Markdown file or a zip of them in, the whole notebook out,
/// and — while a note is open — a picture into it and the note out as a file of its own. They
/// stand in the window's bar, where the controls of the page beside the tree stand, so the page
/// keeps no file control of its own and every command has one place at every width.
view notebook_menu(nb: Notebook, open: Option<Note>) {
    ui.MenuButton("File", icon: .file_plus) {
        ui.MenuItem("Import Markdown…", icon: .file_plus) {
            on choose: file.choose(import_markdown(nb), accept: ["text/markdown"])
        }
        ui.MenuItem("Import a zip of Markdown…", icon: .file_plus) {
            on choose: file.choose(import_archive(nb), accept: ["application/zip"])
        }
        ui.MenuItem("Export the notebook…", icon: .download) {
            on choose: file.save(export_notebook(nb), name: "Notes.zip")
        }
        if let n = open {
            ui.MenuItem("Insert an image…", icon: .image) {
                on choose: file.choose(
                    insert_image(n),
                    accept: ["image/png", "image/jpeg", "image/webp"],
                )
            }
            ui.MenuItem("Export this note…", icon: .download) {
                on choose: file.save(export_note(n), name: export_name(n))
            }
        }
    }
}

/// The control a row shows while it is hovered: a note inside it.
view add_child(n: Note) {
    ui.Button("New sub-note", icon: .plus, icon_only: true) {
        on press: new_child(notebook, n)
    }
}

/// A note's row in the tree: a pin when the person pinned it.
view note_node(n: Note) {
    ui.TreeViewRow(icon: if n.pinned { .pin } else { .file_text }, tone: .secondary)
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

/// The open note: its title, where it stands and when it was written, its tags as chips, its
/// body, what links to it, its discussion and its history. Its body's `[[` writes a link to
/// another of the notebook's notes.
view note_page(n: Note) {
    let nb = root Notebook
    state linking: Bool = false
    state link_text: Text = ""
    // The page's blocks are groups of their own, a group gutter apart: the head, the body, what
    // links here, the discussion and the history. The title, where the note stands and what it
    // holds are one head, closer together.
    stack(gap: .group) {
        stack(gap: .related) {
            ui.TextInput(bind: n.title, title: true)
            note_details(n)
            if not n.tags.is_empty() {
                row(gap: .related, wrap: true) {
                    for t in n.tags {
                        ui.Chip(t) {
                            on remove: untag(n, t)
                        }
                    }
                }
            }
        }
        // What `[[` offers: the notebook's notes that hold what is typed in it, asked of the
        // store as it is typed and handed a page at a time, and none at all while the picker is
        // closed.
        ui.BlockEditor(
            bind: n.body,
            placeholder: "Write something",
            links: if linking { nb.notes.matching(link_text).take(48) } else { [] },
        ) {
            on links_open(open): set(linking, open)
            on links_query(text): set(link_text, text)
        }
        std.linked(n, compact: true)
        discussion.thread(n)
        std.history(n)
    }
}

/// What a note says of itself, as one quiet line under its title: the notes it stands under,
/// each opening it, and beside them when it was made, when it was last written and how many
/// words it holds.
view note_details(n: Note) {
    // The notes over `n`, the outermost first, ending at `n` itself: a trail opens every place
    // before its last, so each note it stands under stays a press and nothing of the path is
    // inert. A narrow window keeps the one place above it, the note it stands under.
    let places = if space.compact { above(n).take(1) } else { above(n).sort_by(a -> depth_of(a)) }
    let made = created(n)?.at
    let edited = updated(n)?.at
    let count = n.body.text.words()
    // Written since it was made: one moment says both when it was not.
    let shows_edited = edited != .none and edited != made
    row(gap: .related, align: .center) {
        // The trail takes the room the facts leave, a line under the title rather than the
        // window's own: what the page says of the note stands with the note.
        if not places.is_empty() {
            stack(fill: 1.0) {
                ui.Breadcrumb {
                    for a in places {
                        ui.BreadcrumbSegment(meta(a).title) {
                            on activate: open(a)
                        }
                    }
                    ui.BreadcrumbSegment(meta(n).title)
                }
            }
        }
        // The facts are as wide as their words (`fill: 0`), so the trail takes the room they
        // leave and elides its middle where that is not enough, never the other way round.
        // One fact follows the next after the separator every caption line uses, as history
        // writes "You created it · Jun 22".
        row(gap: .tight, align: .center, fill: 0.0) {
            // When it was made, and when it was written since: one moment says both.
            if let at = made {
                row(gap: .tight) {
                    text("Created", role: .caption, secondary: true)
                    ui.Time(at, role: .caption, secondary: true)
                }
            }
            if shows_edited {
                if made != .none {
                    ui.Text("·", role: .caption, secondary: true)
                }
                if let at = edited {
                    row(gap: .tight) {
                        text("Edited", role: .caption, secondary: true)
                        ui.Time(at, role: .caption, secondary: true)
                    }
                }
            }
            // A note nobody has written yet holds no words to count.
            if count > 0 {
                if made != .none or shows_edited {
                    ui.Text("·", role: .caption, secondary: true)
                }
                if count == 1 {
                    text("1 word", role: .caption, secondary: true)
                } else {
                    text("{count} words", role: .caption, secondary: true)
                }
            }
        }
    }
}

/// The notes `n` stands under, the one it stands under first: the path its page shows above it.
fn above(n: Note) -> List<Note> {
    if let p = n.parent { [[p], above(p)].flat_map(notes -> notes) } else { [] }
}

/// The name a note's file is offered as: the Markdown's own while the note shows no picture, and
/// the zip's when it does.
fn export_name(n: Note) -> Text {
    if n.images.keys().is_empty() { "{file_name(n)}.md" } else { "{file_name(n)}.zip" }
}

/// The files one note's export holds: its Markdown at `folder`/`name`, and each picture it shows
/// under that note's own `images` folder, beside the note as a reader of it finds them.
fn note_files(n: Note, name: Text, folder: Text) -> List<File> {
    let page = File.of_text("{folder}{name}.md", note_markdown(n, name))
    let pictures: List<File> = n.images
        .keys()
        .map(
            address -> {
                File.at("{folder}{name}/images/{picture_name(n, address)}", address)
            },
        )
    [[page], pictures].flat_map(files -> files)
}

/// A note as Markdown: its title, then its body, each picture it shows standing under `name`'s own
/// `images` folder — a path relative to the note, as a reader of the file follows it.
fn note_markdown(n: Note, name: Text) -> Text {
    let body = n.body.markdown.trim()
    for address in n.images.keys() {
        body = body.replace("]({address})", "]({name}/images/{picture_name(n, address)})")
    }
    let title = n.title.trim()
    if title == "" {
        return body
    }
    if body.starts_with("# {title}") {
        return body
    }
    if body == "" {
        return "# {title}"
    }
    "# {title}\n\n{body}"
}

/// The name a picture of `n` stands under in an export: the name it was taken in as, and — while
/// another picture of the note is called that too — its place among them (`fox-2.png`).
fn picture_name(n: Note, address: Text) -> Text {
    let name = base_name(n.images.get(address) ?? "image")
    let keys = n.images.keys()
    let same = keys
        .take(keys.index_of(address) ?? 0)
        .filter(k -> base_name(n.images.get(k) ?? "image") == name)
    if same.is_empty() {
        name
    } else {
        "{stem_of(name)}-{same.len() + 1}{extension_of(name)}"
    }
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

/// The name a note is saved as: its title, a dash for each character a file name cannot keep, and
/// "Note" while its title is nothing a file can be called.
fn file_name(n: Note) -> Text {
    let safe = n.title
        .trim()
        .replace("/", "-")
        .replace("\\", "-")
        .replace(":", "-")
        .replace("\n", "-")
    if safe == "" or safe == "." or safe == ".." { "Note" } else { safe }
}

/// A file's name without its extension: `fox.png` is `fox`.
fn stem_of(name: Text) -> Text {
    let parts = base_name(name).split(".")
    if parts.len() <= 1 { base_name(name) } else { parts.take(parts.len() - 1).join(".") }
}

/// The extension a file's name gives it, the dot included: `.png` for `fox.png`, and nothing.
fn extension_of(name: Text) -> Text {
    let parts = base_name(name).split(".")
    if parts.len() <= 1 { "" } else { ".{parts.last() ?? ""}" }
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
/// the page: a path a reader of the file follows becomes the address of the picture's bytes, which
/// every device of the workspace holds.
fn pictures_shown(markdown: Text, page: Text, pictures: List<File>) -> Text {
    let shown = markdown
    let folder = folder_of(page)
    for picture in pictures {
        if let at = page_ref(folder, picture.name) {
            if shown.contains("]({at})") {
                shown = shown.replace("]({at})", "]({picture.url})")
            }
        }
    }
    shown
}

/// `path` as a page in `folder` names it: the files beside it by their own name, and those under
/// it by the path from it (`images/fox.png` for a page standing in `Q3 plan`). What a page cannot
/// reach from where it stands it does not name.
fn page_ref(folder: Text, path: Text) -> Option<Text> {
    if folder == "" {
        return path
    }
    if path.starts_with("{folder}/") {
        return path.slice(folder.len() + 1, path.len())
    }
    .none
}

/// The note that ends the run `t` begins: the last note under it, or `t` itself.
fn last_below(notes: List<Note>, t: Note) -> Option<Note> {
    .some(notes.descendants(t, .parent).last() ?? t)
}

/// How deep `n` stands in the notebook: the notes it is under, counted.
fn depth_of(n: Note) -> Int {
    if let p = n.parent { 1 + depth_of(p) } else { 0 }
}

/// Whether `n` holds notes of its own, which the tree shows folded away or open.
fn holds_any(nb: Notebook, n: Note) -> Bool {
    nb.notes.filter(x -> x.parent == n).count() > 0
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

/// A note under another, open: the trail above it says where it stands.
scenario "a nested note" -> Note {
    let home = notebook.notes.insert(
        Note(title: "Home", body: RichText("What the flat needs, and what we cook in it.")),
    )
    notebook.notes.insert(
        Note(title: "Groceries", body: RichText("Oat milk, eggs, two lemons."), parent: home),
    )
}

/// A note that embeds another one, open: the embedded note stands in it as a card of its title
/// and the start of what it says.
scenario "a note with an embed" -> Note {
    let home = notebook.notes.insert(
        Note(title: "Home", body: RichText("What the flat needs, and what we cook in it.")),
    )
    let groceries = notebook.notes.insert(
        Note(
            title: "Groceries",
            body: RichText(
                "Oat milk, eggs, two lemons.\nBread and cheese for lunch.\nCoffee beans for the week.\nApples for the crumble.\nYoghurt, the plain one.\nSomething green for Sunday.",
            ),
            parent: home,
        ),
    )
    notebook.notes.insert(
        Note(
            title: "Saturday shopping",
            body: RichText("What to take to the market:\n", RichText.embed(groceries)),
            parent: home,
        ),
    )
}

/// A page written in sections: a note whose body is headings two levels under its title, each
/// with the prose under it.
scenario "a note with headings" -> Note {
    notebook.notes.insert(
        Note(
            title: "Autumn plan",
            body: RichText.from_markdown(
                "## Ship the editor\n\nOffline first, then sharing: nobody waits on a network to write.\n\n### Then\n\nThe importer, the table editor and the find bar.\n\n## Grow the garden\n\nTomatoes along the south wall, herbs by the door.\n\n### Seeds to order\n\nBasil, coriander, two kinds of chilli.",
            ),
        ),
    )
}
 
