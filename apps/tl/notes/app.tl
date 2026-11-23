app so.tilo.notes "Notes" (
    icon: .file_text,
    category: .office,
    tagline: "Notes you write alone or together",
    keywords: ["note", "memo", "memos", "notebook", "journal", "markdown", "notepad", "textedit", "apple notes", "google keep", "onenote", "obsidian", "notiz", "notizbuch", "aufzeichnungen", "zettel", "merkzettel", "tagebuch", "notizen machen"],
)

use so.tilo.lang.discussion as discussion

/// A note: a title, a body, the note it stands under, the tags people gave it and the
/// reactions they left on it.
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
    /// Who reacted to the note, by emoji: the person and agent each pill in its header stands
    /// for, and whose own reaction a press on it takes back.
    reactions: Map<Set<Actor>>
    /// The reactions it had in the native Notes app, each emoji with how many reacted with it:
    /// who reacted there is not carried, since this app names people only as its own members.
    reacted: Text = ""
    /// The file the note was imported from, by the address of its content: importing the same
    /// file again opens this note rather than making a second one.
    source: Text = ""
    /// The name that file had, path inside its archive included: an import that keeps a
    /// package's nesting finds the note a page stands under by it.
    origin: Text = ""
    /// Who wrote the page where it came from, when another service held it before this
    /// notebook: kept with the note so a page brought over still says whose words it is.
    by: Text = ""
    /// The service that page came from, when it came from one (`Outline`).
    service: Text = ""
    /// Where that page is read there, when it came from one: importing the same page again
    /// opens this note rather than making a second one.
    page: Text = ""
    /// When the page was written there, and last changed there, as that service said it.
    created: Text = ""
    updated: Text = ""

    search {
        filter pinned (icon: .pin)
        filter tags
    }
}

/// The workspace's notebook: its notes, in the tree's order.
type Notebook {
    notes: Collection<Note>
    /// Notes carried over from the native Notes app that it no longer showed: a page whose
    /// parent was deleted, kept here rather than dropped, out of the tree.
    set_aside: Collection<Note>
}

root notebook: Notebook

default view notebook for Notebook

default view note_editor for Note

row view note_row for Note

place view notes for Notebook

/// Starts a note at the top of the tree, titled `title` and saying `body` (Markdown) when the
/// caller knows them already. Started from the notebook both are empty: what is typed first
/// names it.
primary action new_note(nb: Notebook, title: Text = "", body: Text = "") -> Note (icon: .plus) {
    nb.notes.insert(Note(title: title, body: RichText.from_markdown(body)))
}

/// Starts a note inside `n`'s own, after the notes it already holds, titled `words` when the
/// caller knows them already (a `/page` in one of `n`'s blocks names the page it makes with the
/// words that stood there): the note the window has open too, so a person writing one starts the
/// note it stands under without finding its row. It is `n`'s action, so the window offers it for
/// the note it shows and its row offers it.
action new_child(n: Note, words: Text = "") -> Note (icon: .plus, label: "New sub-note") {
    let made = notebook.notes.insert(Note(title: words, parent: n))
    if let last = notebook.notes.filter(x -> x.parent == n and x != made).last() {
        notebook.notes.move(made, after: last)
    } else { notebook.notes.move(made, after: n) }
    made
}

/// The notebook the native Notes app kept before this app took its id over, carried into this
/// one once: each page a note, under the note its page stood under and in the order its pages
/// stood there, with its title, its body, its tags and the id its page had. A page the native app
/// no longer showed, because a note above it was deleted, is kept in `set_aside`; a page whose
/// parents run in a circle stands at the top, as the native app showed it. The native app's own
/// documents are left as they are.
migration action carry_native_notebook(nb: Notebook) {
    let earlier = earlier(nb)
    let tags: Map<Text> = Map<Text>()
    for tag in earlier.filter(e -> e.of == "tags") {
        tags.put(tag.id, tag.texts.get("name") ?? "")
    }
    // Siblings stand in the order of their ranks, the same rank by id; a notebook kept before
    // pages had ranks keeps them in the order of its collection.
    let pages = earlier.filter(e -> e.of == "pages").sort_by(e -> e.id).sort_by(e -> placed(e))
    let parents: Map<Text> = Map<Text>()
    for page in pages {
        parents.put(page.id, page.texts.get("parent") ?? "")
    }
    let steps = pages.len() + 1
    let shown = [
        native_tree(pages, ""),
        pages.filter(e -> chain_end(parents, e.id, steps) == "circle"),
    ]
        .flat_map(x -> x)
    // The pictures the native app kept beside its pages, by the digest a page names each by,
    // each now a file of this device's content store.
    let pictures: Map<File> = Map<File>()
    for own in earlier.filter(e -> e.of == "") {
        let held = own.files.get("images") ?? Map<File>()
        for digest in held.keys() {
            if let picture = held.get(digest) { pictures.put(digest, picture) }
        }
    }
    // A link to a native page names it `#page-<id>`: it becomes the link to the note it is.
    let links: Map<Text> = Map<Text>()
    for page in pages {
        links.put("#page-{page.id}", page.id)
    }
    let gone = pages.filter(e -> chain_end(parents, e.id, steps) == "gone")
    // A page comes after the page it stands under, so its parent is carried before it; a page
    // whose parent is gone is set aside.
    let carried: Map<Note> = Map<Note>()
    for page in [shown, gone].flat_map(x -> x) {
        let end = chain_end(parents, page.id, steps)
        let title = page.texts.get("title") ?? ""
        let native = page.rich.get("body") ?? RichText("")
        let markdown = native.markdown
        let body = native.relinked(links: links, sources: picture_sources(markdown, pictures))
        let reacted = reactions_said(page.lists.get("reactions") ?? [])
        let made = if end == "gone" {
            nb.set_aside.insert(Note(title: title, body: body, reacted: reacted), id: page.id)
        } else {
            let parent = if end == "top" {
                carried.get(page.texts.get("parent") ?? "")
            } else { .none }
            nb.notes.insert(
                Note(title: title, body: body, parent: parent, reacted: reacted),
                id: page.id,
            )
        }
        for tag in page.lists.get("tags") ?? [] {
            if let name = tags.get(tag) { made.tags.add(name) }
        }
        // Each picture the page shows is the note's, under the name the page gave it; one whose
        // bytes the native app no longer held is set aside, saying so, where the page showed it.
        for digest in asset_digests(markdown) {
            let alt = picture_alt(markdown, digest)
            if let picture = pictures.get(digest) {
                let ext = picture.name.split(".").last() ?? "bin"
                made.images.put(
                    picture.url,
                    if alt == "" { picture.name } else { base_name("{alt}.{ext}") },
                )
            } else {
                nb.set_aside.insert(
                    Note(
                        title: "A picture of {title}",
                        body: RichText(
                            "The native Notes app kept no bytes for this picture of “{title}”, so it could not be carried over: ![{alt}](asset:{digest})",
                        ),
                    ),
                )
            }
        }
        carried.put(page.id, made)
    }
}

/// Where each picture a native page's `markdown` shows is now, by the source the page named it
/// by (`asset:<digest>`): the address of the file its bytes were kept as.
fn picture_sources(markdown: Text, pictures: Map<File>) -> Map<Text> {
    let sources: Map<Text> = Map<Text>()
    for digest in asset_digests(markdown) {
        if let picture = pictures.get(digest) {
            sources.put("asset:{digest}", picture.url)
        }
    }
    sources
}

/// The digests of the pictures `markdown` shows as the native app named them, `asset:<digest>`.
fn asset_digests(markdown: Text) -> List<Text> {
    digests_from(markdown.split("](asset:"), 1).filter(d -> d != "")
}

/// The digest each of `parts` from the one at `at` starts with, up to the `)` closing it.
fn digests_from(parts: List<Text>, at: Int) -> List<Text> {
    if let part = parts.at(at) {
        [[part.split(")").at(0) ?? ""], digests_from(parts, at + 1)].flat_map(x -> x)
    } else { [] }
}

/// The words `markdown` shows the picture of `digest` with: what its `![...]` says.
fn picture_alt(markdown: Text, digest: Text) -> Text {
    let before = markdown.split("](asset:{digest})").at(0) ?? ""
    before.split("![").last() ?? ""
}

/// Where a native page stands among its siblings: its rank, or its place in its collection for
/// a page kept before pages had ranks.
fn placed(page: host.Earlier) -> Text {
    let rank = page.texts.get("rank") ?? ""
    if rank == "" { page.order } else { rank }
}

/// The native pages under the page `parent` (`""` for the top), each followed by the pages under
/// it: the order this notebook keeps its notes in.
fn native_tree(pages: List<host.Earlier>, parent: Text) -> List<host.Earlier> {
    pages
        .filter(e -> (e.texts.get("parent") ?? "") == parent)
        .flat_map(e -> [[e], native_tree(pages, e.id)].flat_map(x -> x))
}

/// Where the chain of parents above the page `id` ends, within `left` more steps: at the top
/// (`top`), at a page that is gone (`gone`), or nowhere, going round (`circle`).
fn chain_end(parents: Map<Text>, id: Text, left: Int) -> Text {
    if let parent = parents.get(id) {
        if parent == "" {
            "top"
        } else if left <= 0 { "circle" } else { chain_end(parents, parent, left - 1) }
    } else { "gone" }
}

/// The reactions a native page kept, each key naming an emoji and then who reacted with it, as
/// words: each emoji and how many reacted with it, in the order they were first used. Who
/// reacted there is not carried, since this app names people only as its own members.
fn reactions_said(keys: List<Text>) -> Text {
    said_from(keys.map(k -> emoji_of(k)), 0, [])
}

/// [`reactions_said`] of `emoji` from the `at`th, past the emoji already `told`.
fn said_from(emoji: List<Text>, at: Int, told: List<Text>) -> Text {
    if let e = emoji.at(at) {
        if told.contains(e) {
            said_from(emoji, at + 1, told)
        } else {
            let here = "{e} {emoji.count(x -> x == e)}"
            let rest = said_from(emoji, at + 1, [told, [e]].flat_map(x -> x))
            if rest == "" { here } else { "{here}  {rest}" }
        }
    } else { "" }
}

/// The emoji a native reaction's key names: what stands before its first separator. The key
/// ends in who reacted, written in hex, after the last one.
fn emoji_of(key: Text) -> Text {
    let unhexed = without_hex(key)
    let separator = unhexed.slice(unhexed.len() - 1, unhexed.len())
    key.split(separator).first() ?? key
}

/// `key` without the hex digits it ends in.
fn without_hex(key: Text) -> Text {
    if key.len() > 0 and "0123456789abcdef".contains(key.slice(key.len() - 1, key.len())) {
        without_hex(key.slice(0, key.len() - 1))
    } else { key }
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
        nb.notes.place(moved, .parent, before: b)
    } else {
        nb.notes.place(moved, .parent, inside: parent)
    }
}

/// Moves a note to just after `n` and every note under it: the place a drop after a row asks
/// for, which names the row it lands after rather than the row after that one.
action move_after(nb: Notebook, moved: Note, n: Note) (label: "Move to") {
    require not within(nb.notes, n, moved) "A note cannot go inside itself"
    nb.notes.place(moved, .parent, after: n)
}

/// Puts a note under the note it stands over, with the notes under it, as `Tab` does in the
/// tree; with none named, it goes under the note above it.
action indent(n: Note, above: Option<Note> (above)) (
    label: "Indent",
    icon: .arrow_right,
    group: "Move",
) {
    applies above != .none and above != n.parent "Nothing over it to move under"
    notebook.notes.place(n, .parent, under: above)
}

/// Puts a note beside the note it stands in, just after it and the notes under it, as
/// `Shift-Tab` does in the tree.
action outdent(n: Note) (label: "Outdent", icon: .arrow_left, group: "Move") {
    applies n.parent != .none "It is at the top level already"
    if let p = n.parent {
        notebook.notes.place(n, .parent, after: p)
    }
}

/// Puts a note just over the note above it, with the notes under it, as `Alt-Up` does in the
/// tree; with none named, over the note just above it.
action move_up(n: Note, above: Option<Note> (above)) (
    label: "Move up",
    icon: .arrow_up,
    group: "Move",
) {
    applies above != .none "Nothing over it to move it over"
    notebook.notes.place(n, .parent, up: above)
}

/// Puts a note just under the note below it and the notes under that one, with its own notes
/// after it, as `Alt-Down` does in the tree.
action move_down(n: Note) (label: "Move down", icon: .arrow_down, group: "Move") {
    notebook.notes.place(n, .parent, down: .none)
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

/// Writes `text` (Markdown) at the end of a note, under what it says already: a line, a list or
/// a paragraph added to it.
action add_text(n: Note, text: Text) (icon: .pencil, label: "Add text", did: "added to a note") {
    n.body = RichText(n.body, RichText.from_markdown(text))
}

/// Writes `text` (Markdown) at the end of the note titled `title`, the case of its letters aside:
/// how an agent or a script that knows a note by its name ("add this to Meeting notes") writes
/// into it without first looking up which note that is.
action append_to_note(nb: Notebook, title: Text, text: Text) (
    icon: .pencil,
    label: "Add to a note",
    did: "added to a note",
) {
    require titled(nb, title) != .none "No note is titled {title}"
    if let n = titled(nb, title) {
        n.body = RichText(n.body, RichText.from_markdown(text))
    }
}

/// The note titled `title`, the case of its letters and the spaces around it aside.
fn titled(nb: Notebook, title: Text) -> Option<Note> {
    nb.notes.find(n -> n.title.trim().lower() == title.trim().lower())
}

/// Files a note titled `title` and tagged `tag` in one step: what another app writes down of
/// what it did (Today's morning) without waiting on the note it made to name it.
action file_note(nb: Notebook, title: Text, tag: Text) -> Note (
    icon: .file_plus,
    label: "File a note",
) {
    let made = nb.notes.insert(Note(title: title))
    made.tags.add(tag)
    made
}

/// Gives a note a tag.
action tag(n: Note, t: Text) (icon: .tag, group: "Tags") {
    n.tags.add(t)
}

/// Untags a note.
action untag(n: Note, t: Text) (label: "Untag", group: "Tags") {
    n.tags.remove(t)
}

/// Adds your reaction with an emoji to a note, or takes it back when you had reacted with it: a
/// press on one of its pills, or the pick of the one its add button opened.
action react(n: Note, emoji: Text) (icon: .smile_plus, label: "React") {
    let mine = if let who = n.reactions.get(emoji) { who.any(p -> p == invoker) } else { false }
    if mine { n.reactions.remove(emoji, invoker) } else { n.reactions.add(emoji, invoker) }
}

/// Puts a picture in a note, where its words end: the person picks the file, and the note
/// keeps the address of its content rather than the picture, and the name it came in as.
action insert_image(n: Note, image: File) (icon: .image, did: "added an image") {
    n.body = RichText(n.body, "\n![]({image.url})")
    n.images.put(image.url, base_name(image.name))
}

/// Keeps a picture the editor wrote into a note where the person pasted or dropped it: the note
/// holds the address of its bytes, and the name it came in as, which an export writes beside it.
/// `picture` is the editor's word of it: its name, then the address of its content.
action keep_image(n: Note, picture: List<Text>) (did: "added an image") {
    let address = picture.at(1) ?? ""
    if address != "" {
        n.images.put(address, base_name(picture.at(0) ?? "Pasted image.png"))
    }
}

/// A note as Markdown: its title, then its body, each picture it shows written beside it as
/// `<note>/images/<name>`, a path another reader of the file follows and this app reads again
/// as the address of the picture's bytes.
action export_markdown(n: Note) -> Text (icon: .download) {
    note_markdown(n, file_name(n), "", Map<Text>())
}

/// A note as a file: its Markdown, and the pictures it shows beside it. A note that shows a
/// picture is one zip — `Q3 plan.md` and `Q3 plan/images/fox.png` — because Markdown reaches a
/// picture by a path and by nothing else; a note of words alone is that Markdown itself.
action export_note(n: Note) -> File (icon: .download) {
    if n.images.keys().is_empty() {
        File.of_text("{file_name(n)}.md", export_markdown(n))
    } else {
        File.zip("{file_name(n)}.zip", note_files(n, file_name(n), "", Map<Text>()))
    }
}

/// The notebook as a zip, one Markdown file for each note: a note's own under the folder of the
/// note it stands in, and the pictures it shows in its own `images` folder beside it. That is the
/// shape the import reads, so the notebook comes back as the tree it is, titles, bodies and
/// pictures alike.
action export_notebook(nb: Notebook) -> File (icon: .download) {
    // Every note is named before any file is written: two notes of one title are two files, a
    // note's folder follows the name its note took, and a link reaches the file of the note it
    // names rather than the first note of that title.
    let pages: Map<Text> = Map<Text>()
    let at: Map<Text> = Map<Text>()
    pages_under(nb, .none, "", pages, at)
    File.zip("Notes.zip", files_under(nb, .none, "", pages))
}

/// The notebook as one Markdown document: each note a heading one level deeper than the note it
/// stands under (six at the deepest), its body beneath, and the notes under it after it — the
/// notebook read as one text, in the order the tree keeps it.
action export_notebook_markdown(nb: Notebook) -> File (icon: .download) {
    File.of_text("Notes.md", notebook_markdown(nb))
}

/// Every note of the notebook as one Markdown document, in the order the tree keeps them.
fn notebook_markdown(nb: Notebook) -> Text {
    markdown_under(nb, .none, 1).join("\n")
}

/// The sections of the notes under `parent` and of the notes under them, `depth` deep: each
/// note's own section, then the sections of the notes it holds, one level deeper.
fn markdown_under(nb: Notebook, parent: Option<Note>, depth: Int) -> List<Text> {
    let sections: List<Text> = []
    for kid in kids_of(nb, parent) {
        sections = [
            sections,
            [markdown_of_note(kid, depth)],
            markdown_under(nb, .some(kid), depth + 1),
        ]
            .flat_map(x -> x)
    }
    sections
}

/// One note as its section of the notebook's document: its title a heading `depth` deep — Markdown
/// has six, so a deeper note stands at the sixth — and its body under it when it says anything.
fn markdown_of_note(n: Note, depth: Int) -> Text {
    let hashes = ["#", "##", "###", "####", "#####", "######"].at(depth.min(6) - 1) ?? "#"
    let body = n.body.markdown.trim()
    let title = n.title.trim()
    if body == "" { "{hashes} {title}\n" } else { "{hashes} {title}\n\n{body}\n" }
}

/// Names the notes under `parent`, and the notes under them, in the order the notebook keeps
/// them: `pages` takes the address each note is written at, by the note's id, and `at` the note
/// each address is written for, which is what a link asks for.
fn pages_under(
    nb: Notebook,
    parent: Option<Note>,
    folder: Text,
    pages: Map<Text>,
    at: Map<Text>,
) -> Map<Text> {
    for kid in kids_of(nb, parent) {
        let name = free_name(folder, file_name(kid), at)
        pages.put(kid.id, "{folder}{name}.md")
        at.put("{folder}{name}.md", kid.id)
        pages_under(nb, .some(kid), "{folder}{name}/", pages, at)
    }
    pages
}

/// The notes that stand under `parent`, in the order the notebook keeps them: asked of the
/// store's index of the parent each note names, never by walking the notes beside it.
fn kids_of(nb: Notebook, parent: Option<Note>) -> List<Note> {
    if let p = parent {
        nb.notes.descendants(p, .parent).filter(x -> x.parent == parent)
    } else { nb.notes.filter(x -> x.parent == parent) }
}

/// The name a note whose title gives `base` is written under in `folder`: `base` itself, and —
/// where a note of the folder took it already — the first of `base (2)`, `base (3)`, … that none
/// of them took. Two `Ideas` under one note are `Ideas.md` and `Ideas (2).md`, and a note titled
/// `Ideas (2)` beside them keeps a file of its own too: no note's file stands over another's.
fn free_name(folder: Text, base: Text, at: Map<Text>) -> Text {
    free_from(folder, base, 1, at)
}

/// The first of the names a note whose title gives `base` may take, from the `place`th one on.
fn free_from(folder: Text, base: Text, place: Int, at: Map<Text>) -> Text {
    let name = if place == 1 { base } else { "{base} ({place})" }
    if at.get("{folder}{name}.md") == .none { name } else { free_from(folder, base, place + 1, at) }
}

/// The files of the notes under `parent`, each at `folder`: a note's own, then — at the folder
/// its own name adds — the files of the notes under it. The tree is walked once from the notes
/// that stand at the top, so a notebook of ten thousand notes costs its notes, and not a walk of
/// the whole notebook for each of them.
fn files_under(nb: Notebook, parent: Option<Note>, folder: Text, pages: Map<Text>) -> List<File> {
    kids_of(nb, parent).flat_map(kid -> files_of(nb, pages, folder, kid))
}

/// The files one note of the notebook's export holds at `folder`: its own Markdown and pictures,
/// then the notes under it, at the folder the name it is written under adds.
fn files_of(nb: Notebook, pages: Map<Text>, folder: Text, n: Note) -> List<File> {
    let name = stem_of(base_name(page_path(n, pages)))
    [note_files(n, name, folder, pages), files_under(nb, .some(n), "{folder}{name}/", pages)]
        .flat_map(files -> files)
}

/// The address an export writes `n` at: the one the notebook's export gave it, and the one its
/// title gives it where it is written alone (`export_note`, `export_markdown`).
fn page_path(n: Note, pages: Map<Text>) -> Text {
    pages.get(n.id) ?? "{file_name(n)}.md"
}

/// Imports a Markdown file as a note, or opens the note that file already made: a note keeps the
/// address of the file it came from, so importing the same file twice makes one note. The note
/// stands under the note of the folder its file's path names — `Q3 plan/Ideas.md` under
/// `Q3 plan.md` — and at the top of the tree when its path names no folder. Its title is the
/// document's first heading, or the file's name; the heading it says the title with is not said
/// again in its body.
action import_markdown(nb: Notebook, file: File) -> Note (
    icon: .file_plus,
    did: "imported a note",
    opens: ["text/markdown", "text/plain", .md, .markdown, .txt],
) {
    // A Markdown or plain-text file opens in the notebook with this: Files and the person's own
    // file system hand the file over as the file it is.
    import_page(nb, file, "", "", "", "", "")
}

/// Imports a page another service held as a note, keeping whose words it is and where it stood
/// there: what `tilo migrate` calls for each page it brings over, and what a person's own import
/// of a file does without a service's words. A page imported by its address there again opens the
/// note it made, whatever the page says now; the note stands under the note of the folder its
/// file's path names.
action import_page(
    nb: Notebook,
    file: File,
    by: Text,
    service: Text,
    page: Text,
    created: Text = "",
    updated: Text = "",
) -> Note (icon: .file_plus, label: "Import a page from a service", did: "imported a note") {
    if let imported = nb.notes.find(n -> n.source == file.url or page != "" and n.page == page) {
        return imported
    }
    let named = page_name(file.name)
    let fresh = match file.text() {
        .ok(text) => {
            let title = first_heading(text) ?? named
            Note(
                title: title,
                body: RichText.from_markdown(without_heading(text, title)),
                source: file.url,
                origin: file.name,
                by: by,
                service: service,
                page: page,
                created: created,
                updated: updated,
            )
        }
        // A file that is not text still makes a note, saying what became of it: the person sees
        // the file they chose, and why it is not a note's words.
        .err(why) => {
            Note(
                title: named,
                body: RichText(why),
                source: file.url,
                origin: file.name,
                by: by,
                service: service,
                page: page,
                created: created,
                updated: updated,
            )
        }
    }
    let made = nb.notes.insert(fresh)
    if let parent = nb.notes.find(n -> n.origin == "{folder_of(file.name)}.md") {
        made.parent = parent
        if let last = nb.notes.filter(x -> x.parent == parent and x != made).last() {
            nb.notes.move(made, after: last)
        } else { nb.notes.move(made, after: parent) }
    }
    made
}

/// Imports a zip of pages as notes under the notebook, a page under the page that held it, in one
/// change: undo takes the whole import back. That is this app's own notebook written out, a
/// Notion export, and what `tilo migrate` brings from a wiki: a page per `.md` file, its pictures
/// beside it, and a database beside it as a `.csv` that becomes a table at its page. Each page's
/// title is its first heading, or its file's name; the pictures a page shows are the files it
/// stands beside, a reference read from the page's own folder, and a link to a page of the archive
/// is the reference to the note that page made. A migration says where each page came from in its
/// own file beside the pages, and a page that is imported already keeps the note it made.
action import_archive(nb: Notebook, file: File) (
    icon: .file_plus,
    did: "imported a notebook",
    opens: ["application/zip", .zip],
) {
    // An archive of pages opens in the notebook with this, as one holding a notebook does.
    match file.files() {
        .ok(files) => {
            let names = files.map(f -> f.name)
            let pictures = files.filter(f -> f.media.starts_with("image/"))
            // Where each page of a migration came from, by the file it is written at: a page of
            // an export nobody migrated is at no key of it.
            let came_from = provenance(files)
            // A page under the page it stood under comes after it: its parent is a note then.
            let pages = files
                .filter(f -> f.media == "text/markdown")
                .filter(p -> not p.name.starts_with("__MACOSX/"))
                .sort_by(p -> p.name.split("/").len())
            let fresh = pages.filter(p -> note_of(nb, p, came_from) == .none)
            // Every page makes its note before any page is read: a link reaches the note a page
            // written after it made, whichever of the two stands first in the archive.
            let at: Map<Text> = Map<Text>()
            for page in pages {
                if let existing = note_of(nb, page, came_from) {
                    at.put(page.name, existing.id)
                } else {
                    let record = came_from.get(page.name) ?? ""
                    let named = record_field(record, 1)
                    let title = if named != "" { named } else { page_name(page.name) }
                    let parent = under_note(nb, page, record)
                    let made = nb.notes.insert(
                        Note(
                            title: title,
                            body: RichText(""),
                            parent: parent,
                            source: page.url,
                            origin: page.name,
                            by: record_field(record, 2),
                            service: record_field(record, 3),
                            page: record_field(record, 4),
                            created: record_field(record, 6),
                            updated: record_field(record, 7),
                        ),
                    )
                    at.put(page.name, made.id)
                    if let p = parent {
                        if let last = nb.notes.filter(x -> x.parent == p and x != made).last() {
                            nb.notes.move(made, after: last)
                        } else { nb.notes.move(made, after: p) }
                    }
                }
            }
            // Then what each page says, the tables its export wrote beside it, and the pictures it
            // shows: the files it stands beside, by the name each stood under, which an export of
            // it writes beside it again.
            for page in fresh {
                let record = came_from.get(page.name) ?? ""
                if let made = note_of(nb, page, came_from) {
                    match page.text() {
                        .ok(text) => {
                            // A service's page keeps the title that service gave it; one of an
                            // export is titled as its first heading says.
                            let named = record_field(record, 1)
                            let title = if named != "" {
                                named
                            } else {
                                first_heading(text) ?? page_name(page.name)
                            }
                            let shown = pictures_shown(
                                without_heading(text, title),
                                page.name,
                                pictures,
                            )
                            made.title = title
                            made.body = RichText.from_markdown(
                                shown,
                                links: at,
                                from: folder_at(page.name),
                            )
                        }
                        .err(why) => {
                            made.body = RichText(why)
                        }
                    }
                    // A database exported beside its page comes after what the page says, as the
                    // table its rows are.
                    for database in csv_beside(page.name, names) {
                        if let data = files.find(f -> f.name == database) {
                            let csv = match data.text() {
                                .ok(text) => text,
                                .err(_) => "",
                            }
                            if let table = csv_table(csv) {
                                made.body = RichText(made.body, RichText.from_markdown(table))
                            }
                        }
                    }
                    let shown = made.body.markdown
                    for picture in pictures {
                        if shown.contains(picture.url) {
                            made.images.put(picture.url, base_name(picture.name))
                        }
                    }
                }
            }
        }
        // An archive that does not read says why, in the note it would have filled.
        .err(why) => {
            if nb.notes.find(n -> n.source == file.url) == .none {
                nb.notes.insert(
                    Note(
                        title: stem_of(file.name),
                        body: RichText(why),
                        source: file.url,
                        origin: file.name,
                    ),
                )
            }
        }
    }
}

/// The file an import from another service carries beside its pages, saying where each came
/// from: one record a line, the first naming the format that line is written in.
fn provenance_file() -> Text {
    "tilo-import.tsv"
}

/// Where each page of `files` came from, by the path of the file it is written at: the record the
/// migration's own file carries for it — the page's title there, who wrote it, the service, its
/// address there, the page it stood under, and when it was written and changed there — each
/// field escaped, so a title holding a tab is still one field. Nothing for a file no migration
/// wrote, and nothing for a file that names another format than this one.
fn provenance(files: List<File>) -> Map<Text> {
    let records: Map<Text> = Map<Text>()
    for f in files {
        if f.name == provenance_file() {
            match f.text() {
                .ok(text) => {
                    let lines = text.split("\n")
                    if let first = lines.first() {
                        if first.trim() == "tilo-import 1" {
                            for line in lines {
                                let fields = line.split("\t")
                                if fields.len() >= 8 {
                                    records.put(unescaped(fields.first() ?? ""), line)
                                }
                            }
                        }
                    }
                }
                .err(_) => {}
            }
        }
    }
    records
}

/// The `at`th field of a provenance record `line`, as the migration wrote it; nothing when the
/// record states no such field.
fn record_field(line: Text, at: Int) -> Text {
    let fields = line.split("\t")
    if line == "" or at >= fields.len() {
        return ""
    }
    unescaped(fields.take(at + 1).last() ?? "")
}

/// The note a page of an import already made: the one its file came in as, or — for a page a
/// migration brings — the one its address where it came from made, so a page whose words changed
/// there since is still the same note.
fn note_of(nb: Notebook, page: File, came_from: Map<Text>) -> Option<Note> {
    let address = record_field(came_from.get(page.name) ?? "", 4)
    nb.notes.find(n -> n.source == page.url or address != "" and n.page == address)
}

/// The note a page of an import stands under: the note of the page its record says it stood
/// under there, else the note of the page whose folder its own path names — `Q3 plan/Ideas.md`
/// under `Q3 plan.md`.
fn under_note(nb: Notebook, page: File, record: Text) -> Option<Note> {
    let above = record_field(record, 5)
    if above != "" {
        let known = nb.notes.find(n -> n.page == above)
        if known != .none {
            return known
        }
    }
    nb.notes.find(n -> n.origin == "{folder_of(page.name)}.md")
}

/// `field` with the escapes a migration writes: a backslash, a tab, a line break and a carriage
/// return, each as the two characters that stand for it.
fn unescaped(field: Text) -> Text {
    let out = ""
    let escaped = false
    for ch in field.split("") {
        if escaped {
            if ch == "t" {
                out = "{out}\t"
            } else if ch == "n" {
                out = "{out}\n"
            } else if ch == "r" {
                out = "{out}\r"
            } else {
                out = "{out}{ch}"
            }
            escaped = false
        } else if ch == "\\" {
            escaped = true
        } else {
            out = "{out}{ch}"
        }
    }
    out
}

/// Files dropped on the notebook come in one at a time. A Markdown file becomes a note. An
/// archive becomes the pages it holds, whether the notebook's own written out or another app's
/// export. One action, so undo takes the whole drop back at once.
action import_dropped(nb: Notebook, files: List<File>) (
    icon: .file_plus,
    label: "Import files",
    did: "imported a file",
) {
    for file in files {
        if file.media == "application/zip" {
            import_archive(nb, file)
        } else { import_markdown(nb, file) }
    }
}

/// The title a page of an export is named by before what it says is read: its own file's name —
/// and nothing for a page the export never titled (`Untitled 1a2b3c4d`), which is called what a
/// titleless note is called until its heading names it.
fn page_name(name: Text) -> Text {
    if untitled_name(name) { "" } else { stem_of(name) }
}

/// The folder the page at `path` stands in, as a page's own folder names it: nothing at the top
/// of the archive, and `Home/` for `Home/Ideas.md`.
fn folder_at(path: Text) -> Text {
    let folder = folder_of(path)
    if folder == "" { "" } else { "{folder}/" }
}

/// The notebook's window: the notes as a tree in the sidebar, the one chosen beside it.
view notebook(nb: Notebook) {
    // The note the window shows, kept by the window: the first note until a person chooses
    // another, and the one a link, a breadcrumb or a search shows.
    state open_note: Option<Note> = nb.notes.first()
    // The open note's headings, as its editor reports them, and the heading a person last chose
    // in the map of them: what the page shows of them without reading the whole note.
    state heading_lines: List<Text> = []
    state heading_levels: List<Int> = []
    state reveal: Option<Text> = .none
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
        row: note_node,
        head: notebook_head,
        drop_files: files -> import_dropped(nb, files),
    ) {
        // A file dropped anywhere on the notebook comes in as a note or as the pages of an
        // archive it holds.
        // What the tree cannot hold: the open note's headings under it, and the page beside it.
        below: {
            note_outline(heading_lines, heading_levels, reveal)
        }
        doc: {
            if let n = open_note {
                note_page(n, heading_lines, heading_levels, reveal)
            }
        }
    }
}

/// The open note's headings, under the tree: choosing one shows that heading of the note beside
/// it. Nothing stands there while the note has no headings.
view note_outline(headings: List<Text>, levels: List<Int>, reveal: State<Option<Text>>) {
    std.headings("Outline", headings, levels, reveal)
}

/// The notebook's controls in the sidebar's header, given the note open beside the tree: its own
/// menu, which holds the open note's commands too. What makes a note is the window's primary
/// action, which the bar offers once, so no button of the head's draws it again; a note under
/// the open one is in that menu, and a row's hover offers the same under the note it will stand
/// in (`add_child`).
view notebook_head(open: Option<Note>) {
    notebook_menu(root Notebook, open)
}

/// The notebook's own commands: a Markdown file or a zip of them in, the whole notebook out,
/// and — while a note is open — a note under it, a picture into it and the note out as a file of
/// its own. They stand in the sidebar's header, where the notebook's controls stand, so the page
/// keeps no file control of its own and every command has one place at every width.
view notebook_menu(nb: Notebook, open: Option<Note>) {
    ui.MenuButton("File", icon: .file_plus) {
        ui.MenuItem("Import Markdown…", icon: .file_plus) {
            on choose: file.choose(import_markdown(nb), accept: ["text/markdown"])
        }
        ui.MenuItem("Import a Notion export or a zip of Markdown…", icon: .file_plus) {
            on choose: file.choose(import_archive(nb), accept: ["application/zip"])
        }
        ui.MenuItem("Export the notebook…", icon: .download) {
            on choose: file.save(export_notebook(nb), name: "Notes.zip")
        }
        ui.MenuItem("Export the notebook as one Markdown document…", icon: .download) {
            on choose: file.save(export_notebook_markdown(nb), name: "Notes.md")
        }
        if let n = open {
            ui.MenuItem("New sub-note", icon: .plus) {
                on choose: new_child(n)
            }
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
        on press: new_child(n)
    }
}

/// A note's row in the tree: a pin when the person pinned it.
view note_node(n: Note) {
    ui.TreeViewRow(icon: if n.pinned { .pin } else { .file_text }, tone: .secondary)
}

/// The notes on their own, as a place shows them: each note a row, in the notebook's
/// order, opening beside the list. None of the window's tree, editor or bar is around them.
view notes(nb: Notebook) {
    if nb.notes.is_empty() {
        ui.EmptyState("No notes yet", icon: .file_text)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: Note): open(x, front: false)
            on activate(x: Note): open(x)
            for n in nb.notes {
                ui.ListItem(item: n) {
                    note_row(n)
                }
            }
        }
    }
}

/// One note in a list: its title, the start of its body, and a pin when it is pinned.
view note_row(n: Note) {
    ui.ListRow(title: meta(n).title, secondary: n.body.excerpt, mark: if n.pinned { .pin })
}

/// A note's window of its own: its page, as a document the window is for.
view note_editor(n: Note) {
    state heading_lines: List<Text> = []
    state heading_levels: List<Int> = []
    state reveal: Option<Text> = .none
    std.document(n) {
        note_page(n, heading_lines, heading_levels, reveal)
    }
}

/// The open note: its title, where it stands and when it was written, its tags as chips and its
/// reactions, its body, what links to it and its discussion (its history is the window's, under
/// the window bar's History). Its body's `[[` writes a link to another of the notebook's notes.
/// Its editor reports the note's headings — their words and how deep each stands — which the
/// window's map of them is drawn from, and shows the heading a person chose there.
template view note_page(
    n: Note,
    headings: State<List<Text>>,
    heading_levels: State<List<Int>>,
    reveal: State<Option<Text>>,
) {
    let nb = root Notebook
    state linking: Bool = false
    state link_text: Text = ""
    // The page's blocks are groups of their own, a group gutter apart: the head, the body, what
    // links here and the discussion. The title, where the note stands and what it
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
            // Who reacted to the note, a pill per emoji: a press is the person's own reaction
            // with it, and the add button opens the picker. The reactions it had in the native
            // Notes app stand under them, naming how many reacted with each and no one here.
            ui.Reactions(reactions: n.reactions) {
                on toggle(e): react(n, e)
            }
            if n.reacted != "" {
                text("Reacted in the native Notes: {n.reacted}", role: .caption, secondary: true)
            }
        }
        // What `[[` offers: the notebook's notes that hold what is typed in it, asked of the
        // store as it is typed and handed a page at a time, and none at all while the picker is
        // closed. Its `/` menu offers a picture and a page of this note's own, which only the
        // app can give it: one asks the person for a file, the other makes a note.
        ui.BlockEditor(
            bind: n.body,
            placeholder: "Write something",
            links: if linking { nb.notes.matching(link_text).take(48) } else { [] },
            reveal: reveal,
            slash: ["image", "page"],
        ) {
            on links_open(open): set(linking, open)
            on links_query(text): set(link_text, text)
            // A picture pasted or dropped into the body is in it already, where the caret was:
            // the note keeps its name beside the address its bytes were kept at.
            on image(picture): keep_image(n, picture)
            on slash_image: file.choose(
                insert_image(n),
                accept: ["image/png", "image/jpeg", "image/webp"],
            )
            // A page made from a block is titled with its words, as the words stand: what a
            // person wrote in the block names the page it became.
            on slash_page(words): new_child(n, words)
            on headings(words, levels): set(headings, words).then(set(heading_levels, levels))
        }
        std.linked(n, compact: true)
        std.after_body {
            discussion.thread(n)
        }
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
    // Written since it was made: a note written once says nothing of when.
    let shows_edited = edited != .none and edited != made
    // A page another service held before this notebook arrived here when it was imported: its
    // arrival is a move, which the line says rather than naming it a day it was written.
    let made_said = if n.service == "" { "Created" } else { "Moved here" }
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
        // leave and elides its middle where that is not enough, never the other way round. One
        // fact never breaks: where the line is short of room the whole fact moves down to the
        // next line, beside the trail.
        // The gap separates one fact from the next; no glyph stands between them.
        row(gap: .related, align: .center, fill: 0.0, wrap: true) {
            // A page brought over from another service says where it came from and whose words
            // it is, once, before the facts of this notebook: it was not made here.
            if n.service != "" {
                if n.by == "" {
                    text("From {n.service}", role: .caption, secondary: true)
                } else {
                    text("From {n.service} by {n.by}", role: .caption, secondary: true)
                }
            }
            // When it was made — and, for a page brought here from another service, when it
            // arrived — then when it was written since; who made it is the window's History's
            // to say.
            if let at = made {
                row(gap: .tight) {
                    text(made_said, role: .caption, secondary: true)
                    ui.Time(at, role: .caption, secondary: true)
                }
            }
            if shows_edited {
                if let at = edited {
                    row(gap: .tight) {
                        text("Edited", role: .caption, secondary: true)
                        ui.Time(at, role: .caption, secondary: true)
                    }
                }
            }
            // A note nobody has written yet holds no words to count.
            if count > 0 {
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
fn note_files(n: Note, name: Text, folder: Text, pages: Map<Text>) -> List<File> {
    let page = File.of_text("{folder}{name}.md", note_markdown(n, name, folder, pages))
    let pictures: List<File> = n.images
        .keys()
        .map(
            address -> {
                File.at("{folder}{name}/images/{picture_name(n, address)}", address)
            },
        )
    [[page], pictures].flat_map(files -> files)
}

/// A note as Markdown: its title, then its body, each picture it shows under `name`'s own
/// `images` folder — a path relative to the note, as a reader of the file follows it — and each
/// note it links to a link to that note's page, at the path from the folder it is written in.
fn note_markdown(n: Note, name: Text, folder: Text, pages: Map<Text>) -> Text {
    let body = n.body.markdown(links: pages_linked(n, pages), from: folder).trim()
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

/// The name a note is saved as: its title, a dash for each character a file name cannot keep — a
/// path's own, a heading's, and the brackets a Markdown link writes its path in — and "Note"
/// while its title is nothing a file can be called.
fn file_name(n: Note) -> Text {
    let safe = n.title
        .trim()
        .replace("/", "-")
        .replace("\\", "-")
        .replace(":", "-")
        .replace("\n", "-")
        .replace("<", "-")
        .replace(">", "-")
    if safe == "" or safe == "." or safe == ".." { std.say("Note") } else { safe }
}

/// The page each note `n` links to is written at, by the note's id: what its links name, so a
/// reader of the file follows one to the file of the note it spoke of — the one its title gave
/// it, or the place it took among the notes of that title.
fn pages_linked(n: Note, pages: Map<Text>) -> Map<Text> {
    let links: Map<Text> = Map<Text>()
    for referred in n.body.references {
        if let path = pages.get(referred.id) { links.put(referred.id, path) }
    }
    links
}

/// The name at the end of a path: a file's own name, not the folders it stands in
/// (`Notes/Meeting.md` is `Meeting.md`).
fn base_name(path: Text) -> Text {
    path.split("/").last() ?? path
}

/// The folder a path stands in (`Notes/Meeting.md` is `Notes`), and nothing at the top.
fn folder_of(path: Text) -> Text {
    let parts = path.split("/")
    if parts.len() <= 1 { "" } else { parts.take(parts.len() - 1).join("/") }
}

/// A file's name without its extension: `fox.png` is `fox`, and `a.plan.md` is `a.plan`.
fn stem_of(name: Text) -> Text {
    let parts = base_name(name).split(".")
    if parts.len() <= 1 { base_name(name) } else { parts.take(parts.len() - 1).join(".") }
}

/// The extension a file's name gives it, the dot included: `.png` for `fox.png`, and nothing.
fn extension_of(name: Text) -> Text {
    let parts = base_name(name).split(".")
    if parts.len() <= 1 { "" } else { ".{parts.last() ?? ""}" }
}

/// Whether an export named the page at `name` one it never titled: Notion writes
/// `Untitled <id>`, a run of hex digits after the word, for a page the person left without a
/// title, and what the page says names it instead.
fn untitled_name(name: Text) -> Bool {
    let stem = stem_of(name)
    if not stem.starts_with("Untitled ") {
        return false
    }
    let rest = stem.slice("Untitled ".len(), stem.len())
    rest.len() >= 8 and hex_digits(rest)
}

/// Whether `text` is only hex digits: the run an export names an untitled page by.
fn hex_digits(text: Text) -> Bool {
    for ch in text.split("") {
        if not "0123456789abcdefABCDEF".contains(ch) {
            return false
        }
    }
    true
}

/// The data files an export wrote beside the page at `page`: the `.csv` files of its own folder
/// called what the page is called, or that and one more word (`Q3 plan Tasks.csv`, the database
/// `Tasks` of the page `Q3 plan`). In name order, so the same export reads the same way twice.
fn csv_beside(page: Text, names: List<Text>) -> List<Text> {
    let folder = folder_of(page)
    let stem = stem_of(page)
    let beside = names.filter(
        n -> folder_of(n) == folder
            and extension_of(n).lower() == ".csv"
            and database_of(stem_of(n), stem),
    )
    beside.sort_by(n -> n)
}

/// Whether the file called `name` is a database of the page called `stem`: the page's own name,
/// or that and one more word (`Tasks`, `Tasks 2`).
fn database_of(name: Text, stem: Text) -> Bool {
    name == stem or name.starts_with("{stem} ") or name.starts_with("{stem}_")
}

/// `csv` as a Markdown table: its first row the header, then one line for each row after it. A
/// quoted field keeps the commas and line breaks it holds, as RFC 4180 has them; a table's cell
/// writes a line break as a space and a `|` escaped, so the cell ends where it should, and a row
/// of nothing is left out. `.none` when the file states no field at all, or holds more than a
/// note's table takes (a database of 64 000 characters is a page of a document, not a note).
fn csv_table(csv: Text) -> Option<Text> {
    if csv.len() > 64000 {
        return .none
    }
    let rows = csv_rows(csv)
    let header = rows.first() ?? []
    if header.is_empty() {
        return .none
    }
    let width = header.len()
    let table = "{table_row(header, width)}\n{table_rule(width)}\n"
    let heading = true
    for row in rows {
        if heading {
            heading = false
        } else if not empty_row(row) {
            table = "{table}{table_row(row, width)}\n"
        }
    }
    .some(table)
}

/// The rows `csv` holds, each its fields: comma apart, and a quoted field kept whole — its commas,
/// its line breaks, and `""` as the one quote it writes.
fn csv_rows(csv: Text) -> List<List<Text>> {
    let rows: List<List<Text>> = []
    let row: List<Text> = []
    let field = ""
    let quoted = false
    let just_closed = false
    for ch in csv.split("") {
        if quoted {
            if ch == "\"" {
                quoted = false
                just_closed = true
            } else {
                field = "{field}{ch}"
            }
        } else if just_closed and ch == "\"" {
            // `""` inside a quoted field writes one quote, and the quote goes on.
            field = "{field}\""
            quoted = true
            just_closed = false
        } else {
            just_closed = false
            if ch == "\"" {
                quoted = true
            } else if ch == "," {
                row = [row, [field]].flat_map(fields -> fields)
                field = ""
            } else if ch == "\n" {
                row = [row, [field]].flat_map(fields -> fields)
                if not empty_row(row) {
                    rows = [rows, [row]].flat_map(held -> held)
                }
                row = []
                field = ""
            } else if ch != "\r" {
                field = "{field}{ch}"
            }
        }
    }
    row = [row, [field]].flat_map(fields -> fields)
    if not empty_row(row) {
        rows = [rows, [row]].flat_map(held -> held)
    }
    rows
}

/// Whether `row` states nothing: no field, or only fields of space.
fn empty_row(row: List<Text>) -> Bool {
    row.filter(field -> not field.trim().is_empty()).is_empty()
}

/// One row of a Markdown table `width` cells wide: each field as a cell, and a missing field an
/// empty one, so every row of a table ends where its header does.
fn table_row(fields: List<Text>, width: Int) -> Text {
    let cells = padded_cells(fields.take(width).map(field -> table_cell(field)), width)
    let joined = cells.join("|")
    "|{joined}|"
}

/// A Markdown table's rule, `width` cells wide: one run of dashes under each header cell.
fn table_rule(width: Int) -> Text {
    let cells = padded_cells([], width).map(cell -> " --- ")
    let joined = cells.join("|")
    "|{joined}|"
}

/// `cells` made `width` of them: an empty cell for each the row holds fewer than the table is
/// wide.
fn padded_cells(cells: List<Text>, width: Int) -> List<Text> {
    if cells.len() >= width {
        cells
    } else {
        padded_cells([cells, [" "]].flat_map(held -> held), width)
    }
}

/// A Markdown table cell of `field`: its pipe escaped, its line breaks written as spaces, and a
/// space at each side of it.
fn table_cell(field: Text) -> Text {
    let escaped = field.trim().replace("\\", "\\\\").replace("|", "\\|")
    let flat = escaped.replace("\n", " ").replace("\r", " ")
    " {flat} "
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
/// it by the path from it (`images/fox.png` for a page in `Q3 plan`). What a page cannot
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
    let ada = person("Ada")
    let leo = person("Leo")
    invoke(react(plan, "🎉"), by: ada, at: 2026-06-29T07:05Z)
    invoke(react(plan, "🎉"), by: leo, at: 2026-06-29T07:06Z)
    invoke(react(plan, "🚀"), by: leo, at: 2026-06-29T07:06Z)
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

/// A page brought over from another service, open: the line under its title says where it came
/// from and whose words it is, before the facts of this notebook.
scenario "a page from another service" -> Note {
    notebook.notes.insert(
        Note(
            title: "Q3 plan",
            body: RichText("Ship the offline editor, then sharing."),
            by: "Ada",
            service: "Outline",
            page: "https://outline.example/doc/q3-plan",
            created: "2025-08-01T10:00:00Z",
            updated: "2025-09-01T10:00:00Z",
        ),
    )
}

/// A notebook just started: one note, half written.
scenario "a first note" -> Note {
    notebook.notes.insert(
        Note(
            title: "Ideas for the garden",
            body: RichText("Tomatoes along the south wall, herbs by the API."),
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
                "## Ship the editor\n\nOffline first, then sharing: nobody waits on a network to write.\n\n### Then\n\nThe importer, the table editor and the find bar.\n\n## Grow the garden\n\nTomatoes along the south wall, herbs by the API.\n\n### Seeds to order\n\nBasil, coriander, two kinds of chilli.",
            ),
        ),
    )
}

/// The notebook whose open note says its sections: the tree of notes, and under it the outline
/// of the note open beside them.
scenario "the notebook and a note's outline" -> Notebook {
    notebook.notes.insert(
        Note(
            title: "Autumn plan",
            body: RichText.from_markdown(
                "## Ship the editor\n\nOffline first, then sharing.\n\n## Grow the garden\n\nTomatoes along the south wall.",
            ),
        ),
    )
    notebook
}

/// A notebook nobody has written in yet, as a first window opens on it: the window says what it
/// holds none of, once, and the bar offers the way to start the first note.
scenario "an empty notebook" -> Notebook {
    notebook
}
