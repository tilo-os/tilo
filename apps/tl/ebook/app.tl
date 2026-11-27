app so.tilo.ebook "Books" (
    icon: .file_text,
    category: .office,
    tagline: "Reads EPUB books, keeping your place",
    keywords: ["book", "books", "ebook", "ebooks", "epub", "read", "reading", "reader", "novel", "library", "highlights", "annotations", "buch", "buecher", "bücher", "lesen", "roman", "bibliothek"],
    web: ["gutendex.com", "www.gutenberg.org", "gutenberg.org"],
)

use so.tilo.session as session
use tilo.workspace as workspace

/// A book on the shelf: what its package says about it, where the person is in it, its chapters
/// in reading order, and what they marked in it.
type Book (untitled: "Untitled book") {
    title: Text
    author: Text
    subject: Text
    /// The language the book is written in, for hyphenation.
    language: Text = "en"
    /// The address of the file's content: opening the same file again opens this book.
    source: Text
    /// The address of its cover's picture, when the package carries one.
    cover: Text = ""
    added: Date
    chapters: Collection<Chapter>
    marks: Collection<Mark>
    /// Where each member is in the book: one record per reader, changed as they read and kept
    /// with its history, so what a person read follows them across their devices and no one
    /// else's reading shows in theirs.
    progress: Collection<Progress>
}

/// One chapter of a book: what the contents call it, and its words.
type Chapter (untitled: "Chapter") {
    title: Text
    body: RichText
    /// The book it is a chapter of: a search that finds a passage opens it with the book a step
    /// away.
    book: Option<Book>
}

/// A passage a person marked, as the anchor a page shows it at: the block it is in, its first
/// and last character in that block's words, and the words themselves. A bookmark anchors to
/// where a page begins; a note and a comment carry what they say; a comment stands on the
/// highlight it is about.
type Mark (title: excerpt) {
    /// `bookmark`, `highlight`, `note` or `comment`.
    kind: Text
    chapter: Int
    block: Int
    start: Int
    end: Int
    excerpt: Text
    color: Text = "#f5d76e"
    /// What a note or a comment says.
    text: RichText
    created: Date
    /// The highlight a comment is about, by its id.
    on: Text = ""
    /// The book the mark is in: a page of it stands one step from the passage.
    book: Option<Book>
    /// The ranges the passage covers, in reading order, each `block, start, end` in threes: one
    /// block's words, or several when it crosses a block.
    pieces: List<Int>

    search {
        filter "Highlights" = kind == "highlight" (icon: .highlighter)
        filter "Notes" = kind == "note" or kind == "comment" (icon: .pencil)
        filter "Bookmarks" = kind == "bookmark" (icon: .bookmark)
    }
}

/// Where one member is in a book.
type Progress {
    /// The reader this record is: each member's is their own.
    who: Actor
    /// The chapter open, counted from 0, the block its page begins at, the page in it, and how
    /// far down the column the continuous mode stands (`0` its top to `1` its foot).
    chapter: Int
    block: Int
    page: Int
    place: Float
    /// Whether they finished the book: what the shelf leaves out when it hides finished books.
    finished: Bool = false
    /// When they last read on.
    at: Date
}

/// A page palette a reader can pick: its paper, its ink and the colour a selection takes.
type Scheme (title: name) {
    name: Text
    page: Text
    ink: Text
    accent: Text
    /// A palette the reader ships with: the settings keep it and offer no way to delete it.
    builtin: Bool = false
}

/// A face the page can be set in: a family this device has, the name a person gave it, how
/// large it reads beside the others, and whether it shows in the reader's picker.
type Font (title: name) {
    /// The family the face is set in: `serif`, `mono`, a family the toolkit ships, or one the
    /// device has.
    family: Text
    /// The name a person gave it; empty while it is called what its family is.
    name: Text = ""
    /// The text size this face reads at, a multiple of the page's own, between 0.7 and 1.4.
    size: Float = 1.0
    /// Whether it appears in the reader's face picker.
    shown: Bool = true
    /// A face the reader ships with: the settings keep it and offer no way to remove it.
    builtin: Bool = false

    search {
        // A face is the reader's setting, not something the workspace holds to find.
        hide family
        hide name
    }
}

/// A book Project Gutenberg answered a search with, waiting to be downloaded.
type Found (title: title) {
    title: Text
    author: Text
    /// The address its EPUB reads at.
    url: Text
    language: Text
    /// The words it was found for, so a new search takes the old answers away.
    query: Text
}

/// A folder on a device the library watches: every EPUB in it, now or later, becomes a book.
type Watched (title: path) {
    path: Text
    /// The watch this device keeps, so a change to the folder runs the library's scan.
    token: Text = ""
    /// The paths it already gave books for, so a scan reads only what is new.
    given: List<Text>
    added: Date
}

/// The workspace's library: the books on the shelf, the books an online search found, the page
/// palettes, and how a page is set for the people reading here.
type Library {
    books: Collection<Book>
    found: Collection<Found>
    schemes: Collection<Scheme>
    /// The text's size as a multiple of the reading base, between 0.8 and 2.
    text_size: Float = 1.0
    /// The line spacing as a multiple of the face's own, between 0.95 and 1.6.
    line_spacing: Float = 1.2
    /// The face the page is set in.
    face: Text = "serif"
    /// Whether paragraphs are justified like a book's rather than set ragged.
    justified: Bool = false
    /// The palette chosen, by name; the sepia page where it names none.
    scheme: Text = "Sepia"
    /// The faces the page can be set in: the ones the reader ships with, and the families a
    /// person added from this device.
    fonts: Collection<Font>
    /// The name a palette of the person's own is made under, and its paper and ink colours.
    draft_scheme: Text = ""
    custom_page: Text = "#f5ead6"
    custom_ink: Text = "#33291d"
    /// The folder the settings' field names: what "Watch it" watches.
    draft_folder: Text = ""
    /// The folders this device watches for books.
    folders: Collection<Watched>
    /// How the shelf orders its books: `title`, `author` or `added`, the last added first.
    sort_by: Text = "title"
    /// Whether the shelf leaves out the books a person finished.
    hide_finished: Bool = false
}

root library: Library

default view library for Library

default view book_page for Book

default view chapter_page for Chapter

default view mark_page for Mark

row view book_row for Book

row view mark_row for Mark

/// What a new library starts with: the page palettes every reader knows, and the sepia page
/// chosen.
initial action fill(lib: Library) {
    lib.schemes.insert(
        Scheme(name: "Light", page: "#ffffff", ink: "#1b1b1f", accent: "#3a7bd5", builtin: true),
    )
    lib.schemes.insert(
        Scheme(name: "Sepia", page: "#f5ead6", ink: "#33291d", accent: "#9a6b2f", builtin: true),
    )
    lib.schemes.insert(
        Scheme(name: "Dark", page: "#17171c", ink: "#e8e4da", accent: "#8fb8ff", builtin: true),
    )
    lib.schemes.insert(
        Scheme(
            name: "High contrast",
            page: "#000000",
            ink: "#ffffff",
            accent: "#ffe100",
            builtin: true,
        ),
    )
    // The faces every reader knows: the toolkit's reading faces, the plain serif, sans and
    // monospace, and the sizes that set each at the default face's apparent size.
    lib.fonts.insert(Font(family: "serif", size: 1.05, builtin: true))
    lib.fonts.insert(Font(family: "sans", size: 1.0, builtin: true))
    lib.fonts.insert(Font(family: "mono", size: 0.96, builtin: true))
    lib.fonts.insert(Font(family: "libertinus", size: 1.05, builtin: true))
    lib.fonts.insert(Font(family: "literata", size: 0.9, builtin: true))
    lib.fonts.insert(Font(family: "garamond", size: 1.0, builtin: true))
    lib.fonts.insert(Font(family: "source_serif", size: 0.95, builtin: true))
}

/// Adds the EPUB `file` to the shelf and opens it. Its title, author and language are the book's
/// own; each chapter becomes a resource holding its words, so the book is searchable and a
/// reader opens the chapter they are at.
primary action open_book(lib: Library, file: File) -> Book uses time (
    icon: .file_plus,
    opens: ["application/epub+zip", .epub],
    did: "added a book",
) {
    if let held = lib.books.find(b -> b.source == file.url) {
        return held
    }
    let facts = file.book_info()
    let cover = match file.book_cover() {
        .some(picture) => picture.url,
        .none => "",
    }
    let book = lib.books.insert(
        Book(
            title: facts.get("title") ?? file.name,
            author: facts.get("author") ?? "",
            subject: facts.get("subject") ?? "",
            language: facts.get("language") ?? "en",
            source: file.url,
            cover: cover,
            added: today,
        ),
    )
    for part in file.book() {
        let title = part.text.split("\n").first() ?? ""
        book.chapters.insert(Chapter(title: title, body: part, book: book))
    }
    book
}

/// Adds the EPUB files a person dropped on the shelf, each as `open_book` adds it.
action drop_books(lib: Library, files: List<File>) uses time {
    for file in files {
        let is_book = file.media == "application/epub+zip" or file.name.ends_with(".epub")
        if is_book { open_book(lib, file) }
    }
}

/// Watches the folder `path` for books: each EPUB it holds, and each that arrives, becomes a
/// book, keeping the person's place as one they opened themselves does.
action watch_folder(lib: Library, path: Text) uses files, time (
    icon: .folder_open,
    did: "watched a folder for books",
) {
    require path.trim() != "" "A folder is watched by its name"
    let folder = path.trim()
    if lib.folders.any(f -> f.path == folder) {
        return
    }
    let watched = lib.folders.insert(Watched(path: folder, added: today))
    match File.watch(folder, "foldered") {
        .ok(token) => {
            watched.token = token
        }
        .err(why) => {}
    }
    scan_folder(lib, folder)
}

/// Reads the folder `path`'s EPUBs, giving a book to each file the library does not hold yet.
action scan_folder(lib: Library, path: Text) uses files, time {
    match File.list(path) {
        .ok(entries) => {
            for entry in entries {
                if not entry.folder and entry.name.lower().ends_with(".epub") {
                    if let watched = lib.folders.find(f -> f.path == path) {
                        if not watched.given.contains(entry.path) {
                            watched.given = [[entry.path], watched.given].flat_map(x -> x)
                            match File.read(entry.path) {
                                .ok(bytes) => {
                                    open_book(lib, File.of_bytes(entry.name, bytes))
                                }
                                .err(why) => {}
                            }
                        }
                    }
                }
            }
        }
        .err(why) => {}
    }
}

/// Reads every watched folder again: what the platform runs when one of them changes.
action foldered(lib: Library) uses files, time {
    for folder in lib.folders {
        scan_folder(lib, folder.path)
    }
}

/// Takes a book off the shelf, its chapters and marks with it. Reading it again adds it again
/// from its file.
destructive action remove_book(b: Book) (did: "removed a book") {
    library.books.remove(b)
}

/// Gives `b` the title a person reads it by.
action set_title(b: Book, title: Text) (did: "renamed a book") {
    require title.trim() != "" "A book has a title"
    b.title = title.trim()
}

/// Gives `b` its author, or leaves it without one.
action set_author(b: Book, author: Text) (did: "changed a book's author") {
    b.author = author.trim()
}

/// Gives `b` the subject line its card and its shelf filters say.
action set_subject(b: Book, subject: Text) (did: "changed a book's subject") {
    b.subject = subject.trim()
}

/// Sets the language `b` is set in, for its hyphenation.
action set_language(b: Book, language: Text) (did: "changed a book's language") {
    require language.trim() != "" "A book is set in a language"
    b.language = language.trim()
}

/// Puts `picture` on `b` as its cover.
action set_cover(b: Book, picture: File) (did: "changed a book's cover") {
    b.cover = picture.url
}

/// Takes a cover off `b`, so its card draws the book's own art.
action clear_cover(b: Book) (did: "took a book's cover off") {
    b.cover = ""
}

/// Where the person reading is in `b`: their own record, changed in place so its history says
/// where they read and when.
action record_at(b: Book, who: Actor, chapter: Int, block: Int, page: Int, place: Float) uses time {
    if let held = b.progress.find(p -> p.who == who) {
        held.chapter = chapter
        held.block = block
        held.page = page
        held.place = place
        held.at = today
        return
    }
    b.progress.insert(
        Progress(who: who, chapter: chapter, block: block, page: page, place: place, at: today),
    )
}

/// Where the person reading `b` is: their own progress record.
fn my_progress(b: Book, who: Actor) -> Option<Progress> {
    b.progress.find(p -> p.who == who)
}

/// The chapter the person reading `b` is at, counted from 0.
fn my_chapter(b: Book, who: Actor) -> Int {
    match my_progress(b, who) {
        .some(held) => held.chapter,
        .none => 0,
    }
}

/// The block the page the person reading `b` is at begins at, counted from 0.
fn my_block(b: Book, who: Actor) -> Int {
    match my_progress(b, who) {
        .some(held) => held.block,
        .none => 0,
    }
}

/// The page the person reading `b` is at, counted from 0.
fn my_page(b: Book, who: Actor) -> Int {
    match my_progress(b, who) {
        .some(held) => held.page,
        .none => 0,
    }
}

/// How far down the column the person reading `b` stands.
fn my_place(b: Book, who: Actor) -> Float {
    match my_progress(b, who) {
        .some(held) => held.place,
        .none => 0.0,
    }
}

/// Opens `c` in `b`: the contents and where the person is are kept together.
action read_chapter(b: Book, who: Actor, c: Chapter) {
    record_at(b, who, b.chapters.index_of(c) ?? 0, 0, 0, 0.0)
}

/// Shows page `page` of the book's open chapter, counted from 0 (a spread in the two-page mode).
action show_page(b: Book, who: Actor, chapter: Int, block: Int, page: Int) {
    record_at(b, who, chapter, block, page.max(0), 0.0)
}

/// Shows the first page of the chapter `chapter` in `b`: a link to a passage lands on it.
action show_passage(b: Book, who: Actor, chapter: Int, block: Int) {
    require block >= 0 "A passage stands at a block"
    record_at(b, who, chapter.clamp(0, b.chapters.len() - 1), block, 0, 0.0)
}

/// How far down the column the person scrolled.
action show_place(b: Book, who: Actor, chapter: Int, block: Int, page: Int, place: Float) {
    record_at(b, who, chapter, block, page, place.clamp(0.0, 1.0))
}

/// Notes the block the page begins at as the person scrolls the column, so a bookmark lands
/// where they are.
action saw_blocks(b: Book, who: Actor, blocks: List<Int>) uses time {
    if let held = b.progress.find(p -> p.who == who) {
        held.block = blocks.at(0) ?? 0
        held.at = today
    }
}

/// Sets the page's text size, within what a page can be read at.
action set_text_size(lib: Library, size: Float) (did: "changed the text size") {
    lib.text_size = size.clamp(0.8, 2.0)
}

/// Sets the page's line spacing.
action set_line_spacing(lib: Library, spacing: Float) (did: "changed the line spacing") {
    lib.line_spacing = spacing.clamp(0.95, 1.6)
}

/// Sets the face the page is set in.
action set_face(lib: Library, face: Text) (did: "chose the reading face") {
    lib.face = face
}

/// Adds the family `family`, which this device has, to the reader's faces.
action add_font(lib: Library, family: Text) (icon: .plus, did: "added a reading face") {
    require family.trim() != "" "A face has a family"
    let face = family.trim()
    if lib.fonts.any(f -> f.family == face) {
        return
    }
    lib.fonts.insert(Font(family: face, size: 1.0))
}

/// Gives the face `f` the name a person reads it by; empty calls it what its family is.
action rename_font(lib: Library, f: Font, name: Text) (did: "renamed a reading face") {
    f.name = name.trim()
}

/// Sets how large the face `f` reads beside the others.
action set_font_size(lib: Library, f: Font, size: Float) (did: "changed a reading face's size") {
    f.size = size.clamp(0.7, 1.4)
}

/// Says whether the face `f` appears in the reader's picker.
action set_font_shown(lib: Library, f: Font, shown: Bool) (did: "changed whether a face shows") {
    f.shown = shown
}

/// Takes a face of the person's own out of the reader's picker.
destructive action remove_font(lib: Library, f: Font) (did: "removed a reading face") {
    require not f.builtin "The reader's own faces stay"
    // What the face is called is read before it goes: once it is away, nothing of it is left.
    let family = f.family
    lib.fonts.remove(f)
    if lib.face == family {
        lib.face = "serif"
    }
}

/// What a face is called where a person reads it: the name someone gave it, else what its
/// family is.
fn font_title(f: Font) -> Text {
    if f.name.trim() != "" { f.name.trim() } else { font_family_title(f.family) }
}

/// The name a family is known by where a person reads it.
fn font_family_title(family: Text) -> Text {
    if family == "serif" {
        "Serif"
    } else if family == "sans" {
        "Sans"
    } else if family == "mono" {
        "Monospace"
    } else if family == "libertinus" {
        "Libertinus"
    } else if family == "literata" {
        "Literata"
    } else if family == "garamond" {
        "EB Garamond"
    } else if family == "source_serif" { "Source Serif" } else { family }
}

/// The faces the reader's picker offers: the ones a person left shown.
fn shown_fonts(lib: Library) -> List<Font> {
    lib.fonts.filter(f -> f.shown).sort_by(.family)
}

/// The size the face `family` reads at, one for a family nobody tuned.
fn face_size(lib: Library, family: Text) -> Float {
    match lib.fonts.find(f -> f.family == family) {
        .some(f) => f.size,
        .none => 1.0,
    }
}

/// How large the page is set: the text size, times the face's own.
fn reading_scale(lib: Library) -> Float {
    (lib.text_size * face_size(lib, lib.face)).clamp(0.8, 2.0)
}

/// Whether the page's paragraphs are justified.
action set_justified(lib: Library, on: Bool) (did: "changed the paragraph fitting") {
    lib.justified = on
}

/// Orders the shelf by title, by author, or by the books added last.
action set_shelf_sort(lib: Library, order: Text) (did: "sorted the shelf") {
    require order == "title" or order == "author" or order == "added" "A shelf is sorted by title, author or added"
    lib.sort_by = order
}

/// Leaves the books a person finished off the shelf, or shows them again.
action set_hide_finished(lib: Library, hidden: Bool) (did: "changed whether finished books show") {
    lib.hide_finished = hidden
}

/// The books the shelf shows `who`: the order it is sorted in, without the books they finished
/// when it hides those.
fn shelf(lib: Library, who: Actor) -> List<Book> {
    let shown = lib.books.filter(b -> not lib.hide_finished or not finished_by(b, who))
    if lib.sort_by == "author" {
        shown.sort_by(.author)
    } else if lib.sort_by == "added" { shown.sort_by(.added) } else { shown.sort_by(.title) }
}

/// Whether the person reading `b` finished it.
fn finished_by(b: Book, who: Actor) -> Bool {
    match my_progress(b, who) {
        .some(held) => held.finished,
        .none => false,
    }
}

/// Marks `b` finished for the person reading it, or starts it again.
action set_finished(b: Book, who: Actor, finished: Bool = true) uses time (
    icon: .circle_check,
    did: "changed whether a book is finished",
) {
    if let held = b.progress.find(p -> p.who == who) {
        held.finished = finished
        held.at = today
    } else {
        b.progress.insert(
            Progress(
                who: who,
                chapter: 0,
                block: 0,
                page: 0,
                place: 0.0,
                finished: finished,
                at: today,
            ),
        )
    }
}

/// Steps to the next palette the reader ships with: its bar's quick switch.
action cycle_scheme(lib: Library) (icon: .palette, did: "changed the page palette") {
    if lib.scheme == "Sepia" {
        lib.scheme = "Light"
    } else if lib.scheme == "Light" {
        lib.scheme = "Dark"
    } else if lib.scheme == "Dark" {
        lib.scheme = "High contrast"
    } else {
        lib.scheme = "Sepia"
    }
}

/// The palette the page is set in, by name.
action set_scheme(lib: Library, name: Text) (did: "chose the page palette") {
    lib.scheme = name
}

/// A palette of the person's own, from a paper and an ink colour.
action add_scheme(lib: Library, name: Text, page: Text, ink: Text) (did: "made a page palette") {
    require name.trim() != "" "A palette is made under a name"
    lib.schemes.insert(Scheme(name: name, page: page, ink: ink, accent: "#3a7bd5", builtin: false))
}

/// Deletes a palette of the person's own. The four the reader ships with stay.
destructive action delete_scheme(lib: Library, scheme: Scheme) (did: "deleted a page palette") {
    require not scheme.builtin "The reader's own palettes stay"
    // The palette's name is read before it goes: once it is away, nothing of it is left.
    let name = scheme.name
    lib.schemes.remove(scheme)
    if lib.scheme == name {
        lib.scheme = "Sepia"
    }
}

/// The palette `lib` reads in: the one chosen, or the sepia page where it names none.
fn scheme_of(lib: Library) -> Option<Scheme> {
    lib.schemes.find(s -> s.name == lib.scheme) ?? lib.schemes.find(s -> s.name == "Sepia")
}

/// The paper of the page the library reads in.
fn paper_of(lib: Library) -> Text {
    match scheme_of(lib) {
        .some(scheme) => scheme.page,
        .none => "#f5ead6",
    }
}

/// The ink of the page the library reads in.
fn ink_of(lib: Library) -> Text {
    match scheme_of(lib) {
        .some(scheme) => scheme.ink,
        .none => "#33291d",
    }
}

/// The words a passage of `b` holds, at `chapter`, block `block`, characters `start`..`end`.
fn passage_of(b: Book, chapter: Int, block: Int, start: Int, end: Int) -> Text {
    let body = match b.chapters.at(chapter) {
        .some(found) => found.body,
        .none => RichText(""),
    }
    let line = body.text.split("\n").at(block) ?? ""
    line.slice(start, end)
}

/// The lines the chapter at `chapter` is set in: its blocks, one a line.
fn chapter_lines(b: Book, chapter: Int) -> List<Text> {
    match b.chapters.at(chapter) {
        .some(c) => c.body.text.split("\n"),
        .none => [],
    }
}

/// The blocks of the chapter at `chapter` that hold `words`, counted from its top.
fn hits_in(b: Book, chapter: Int, words: Text) -> List<Int> {
    let said = words.trim().lower()
    if said == "" {
        []
    } else {
        hits_from(chapter_lines(b, chapter), said, 0)
    }
}

/// [`hits_in`] from the line at `at`.
fn hits_from(lines: List<Text>, said: Text, at: Int) -> List<Int> {
    if let line = lines.at(at) {
        let here = if line.lower().contains(said) { [at] } else { [] }
        [here, hits_from(lines, said, at + 1)].flat_map(x -> x)
    } else { [] }
}

/// The block the first passage holding `words` stands at, and none.
fn first_hit(b: Book, chapter: Int, words: Text) -> Int {
    hits_in(b, chapter, words).at(0) ?? -1
}

/// The block at `at` of `hits`, and none past its end.
fn hit_at(hits: List<Int>, at: Int) -> Int {
    if at < 0 { -1 } else { hits.at(at) ?? -1 }
}

/// How many passages hold the words the window's search holds.
fn matches_said(count: Int) -> Text {
    if count == 0 {
        "Nothing in this chapter"
    } else if count == 1 { "1 passage" } else { "{count} passages" }
}

/// The ranges a passage covers, each `block, start, end` in threes, as a selection answers them.
fn pieces_of(ranges: List<Int>) -> List<Int> {
    pieces_from(ranges, 0)
}

/// [`pieces_of`] from the range at `at`, keeping the threes a selection gives.
fn pieces_from(ranges: List<Int>, at: Int) -> List<Int> {
    if let block = ranges.at(at), start = ranges.at(at + 1), end = ranges.at(at + 2) {
        [[block, start, end], pieces_from(ranges, at + 3)].flat_map(x -> x)
    } else { [] }
}

/// The bands a mark's ranges draw, each `block:start:end:colour`.
fn bands_of(m: Mark) -> List<Text> {
    bands_from(m.pieces, 0, m.color)
}

/// [`bands_of`] from the range at `at`.
fn bands_from(pieces: List<Int>, at: Int, color: Text) -> List<Text> {
    if let block = pieces.at(at), start = pieces.at(at + 1), end = pieces.at(at + 2) {
        [["{block}:{start}:{end}:{color}"], bands_from(pieces, at + 3, color)].flat_map(x -> x)
    } else { [] }
}

/// The words a passage's ranges hold, each block's words in order, the blocks apart.
fn passage_words(b: Book, chapter: Int, ranges: List<Int>) -> Text {
    words_from(b, chapter, ranges, 0).join(" … ")
}

/// [`passage_words`] from the range at `at`, keeping only the blocks that say something.
fn words_from(b: Book, chapter: Int, ranges: List<Int>, at: Int) -> List<Text> {
    if let block = ranges.at(at), start = ranges.at(at + 1), end = ranges.at(at + 2) {
        let said = passage_of(b, chapter, block, start, end)
        [[said], words_from(b, chapter, ranges, at + 3)]
            .flat_map(x -> x)
            .filter(words -> words != "")
    } else { [] }
}

/// Highlights the passage of `b`'s open chapter: its ranges, each `block, start, end`.
action highlight(b: Book, chapter: Int, ranges: List<Int>, color: Text) uses time (
    did: "highlighted a passage",
) {
    let pieces = pieces_of(ranges)
    b.marks.insert(
        Mark(
            kind: "highlight",
            chapter: chapter,
            block: ranges.at(0) ?? 0,
            start: ranges.at(1) ?? 0,
            end: ranges.at(2) ?? 0,
            excerpt: passage_words(b, chapter, ranges),
            color: color,
            created: today,
            book: .some(b),
            pieces: pieces,
        ),
    )
}

/// Writes a note on the passage of `b`'s open chapter.
action add_note(b: Book, chapter: Int, ranges: List<Int>, color: Text, text: RichText) uses time (
    did: "wrote a note on a passage",
) {
    let pieces = pieces_of(ranges)
    b.marks.insert(
        Mark(
            kind: "note",
            chapter: chapter,
            block: ranges.at(0) ?? 0,
            start: ranges.at(1) ?? 0,
            end: ranges.at(2) ?? 0,
            excerpt: passage_words(b, chapter, ranges),
            color: color,
            text: text,
            created: today,
            book: .some(b),
            pieces: pieces,
        ),
    )
}

/// Bookmarks where the page `block` begins in `b`'s open chapter, or takes the bookmark off
/// again.
action toggle_bookmark(b: Book, chapter: Int, block: Int) uses time (
    did: "bookmarked a page, or took its bookmark off",
) {
    if let held = b.marks.find(
            m -> m.kind == "bookmark" and m.chapter == chapter and m.block == block,
        ) {
        b.marks.remove(held)
        return
    }
    let excerpt = match b.chapters.at(chapter) {
        .some(chapter) => chapter.title,
        .none => "",
    }
    b.marks.insert(
        Mark(
            kind: "bookmark",
            chapter: chapter,
            block: block,
            excerpt: excerpt,
            color: "#f5d76e",
            created: today,
            book: .some(b),
        ),
    )
}

/// Comments on the highlight `m`, in the book `b`.
action comment(b: Book, m: Mark, text: RichText) uses time (did: "commented on a highlight") {
    b.marks.insert(
        Mark(
            kind: "comment",
            chapter: m.chapter,
            block: m.block,
            start: m.start,
            end: m.end,
            excerpt: m.excerpt,
            color: m.color,
            text: text,
            created: today,
            on: m.id,
            book: .some(b),
            pieces: m.pieces,
        ),
    )
}

/// Recolours a highlight, and the notes on it.
action change_color(b: Book, m: Mark, color: Text) (did: "recoloured a highlight") {
    m.color = color
    for note in b.marks.filter(x -> x.on == m.id) {
        note.color = color
    }
}

/// Rewrites what a note or a comment says. Only its author may.
action edit_mark_text(b: Book, m: Mark, text: RichText) (icon: .pencil, label: "Edit a note") {
    require created(m)?.who == invoker "Only its author can edit a note"
    require not text.is_empty() "Write a note first"
    m.text = text
}

/// Copies a passage, so it can be pasted anywhere: what a selection's Copy does.
action copy_passage(text: Text) uses clipboard (icon: .copy, label: "Copy the passage") {
    if text.trim() != "" { clipboard.copy(text) }
}

/// Deletes a mark, and the comments on it.
destructive action delete_mark(b: Book, m: Mark) (did: "deleted a mark") {
    for note in b.marks.filter(x -> x.on == m.id) {
        b.marks.remove(note)
    }
    b.marks.remove(m)
}

/// The comments on `m`.
fn comments_on(b: Book, m: Mark) -> List<Mark> {
    b.marks.filter(x -> x.kind == "comment" and x.on == m.id)
}

/// The text a field of a JSON object holds, or empty where it holds none.
fn text_in(fields: Map<json.Value>, name: Text) -> Text {
    match fields.get(name) {
        .some(value) => match value {
            .string(text) => text,
            .object(inner) => "",
            .array(items) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// The texts an array of a JSON object holds, or none.
fn strings_in(fields: Map<json.Value>, name: Text) -> List<Text> {
    match fields.get(name) {
        .some(value) => match value {
            .array(items) => items
                .map(
                    item -> match item {
                        .string(text) => text,
                        .object(inner) => "",
                        .array(inside) => "",
                        .number(number) => "",
                        .bool(flag) => "",
                        .null => "",
                    },
                )
                .filter(text -> text != ""),
            .object(inner) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
        .none => [],
    }
}

/// The value a key holds inside each object of a JSON array's objects, as `name` reads an
/// author's name.
fn names_in(fields: Map<json.Value>, list: Text, key: Text) -> List<Text> {
    match fields.get(list) {
        .some(value) => match value {
            .array(items) => items
                .map(
                    item -> match item {
                        .object(one) => text_in(one, key),
                        .string(text) => "",
                        .array(inside) => "",
                        .number(number) => "",
                        .bool(flag) => "",
                        .null => "",
                    },
                )
                .filter(text -> text != ""),
            .object(inner) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
        .none => [],
    }
}

/// The address a found book's EPUB reads at, or none where its answer names no EPUB.
fn epub_in(fields: Map<json.Value>) -> Text {
    match fields.get("formats") {
        .some(value) => match value {
            .object(formats) => text_in(formats, "application/epub+zip"),
            .string(text) => "",
            .array(items) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// Searches Project Gutenberg for books matching `query`, taking the answers an earlier search
/// left behind. The books stand under the shelf until a person downloads one.
action find_books(lib: Library, query: Text) uses web (
    icon: .search,
    did: "searched for books",
    search: Found,
) {
    let words = query.trim()
    if words == "" {
        return
    }
    for old in lib.found {
        if old.query != words { lib.found.remove(old) }
    }
    let encoded = words.replace(" ", "+")
    let address = "https://gutendex.com/books/?search={encoded}"
    let answer = web.get(address)
    if not answer.ok {
        return
    }
    match json.parse(answer.text()) {
        .ok(document) => match document {
            .object(answer_fields) => match answer_fields.get("results") {
                .some(results) => match results {
                    .array(books) => {
                        for book in books {
                            match book {
                                .object(fields) => {
                                    let url = epub_in(fields)
                                    if url != "" {
                                        lib.found.insert(
                                            Found(
                                                title: text_in(fields, "title"),
                                                author: names_in(fields, "authors", "name")
                                                    .first(
                                                    )
                                                    ?? "",
                                                url: url,
                                                language: strings_in(fields, "languages").first()
                                                    ?? "en",
                                                query: words,
                                            ),
                                        )
                                    }
                                }
                                .string(text) => {}
                                .array(items) => {}
                                .number(number) => {}
                                .bool(flag) => {}
                                .null => {}
                            }
                        }
                    }
                    .object(fields) => {}
                    .string(text) => {}
                    .number(number) => {}
                    .bool(flag) => {}
                    .null => {}
                },
                .none => {}
            },
            .array(items) => {}
            .string(text) => {}
            .number(number) => {}
            .bool(flag) => {}
            .null => {}
        },
        .err(why) => {}
    }
}

/// Downloads the book an online search found and opens it: the window opens the book the
/// action returns, as it opens what any primary action makes. An answer that did not come
/// leaves the book found, so the person can ask again.
action download_found(lib: Library, f: Found) -> Option<Book> uses web (
    icon: .cloud_download,
    did: "downloaded a book",
) {
    let answer = web.get(f.url)
    if not answer.ok {
        return .none
    }
    let file = File.of_bytes("{f.title}.epub", answer.body)
    let book = open_book(lib, file)
    lib.found.remove(f)
    return .some(book)
}

/// Takes an online search's answers away.
action clear_found(lib: Library) (did: "cleared the books found online") {
    for old in lib.found {
        lib.found.remove(old)
    }
}

/// How far through a book the person is, as a sentence its card and row say.
fn progress_of(b: Book, who: Actor) -> Text {
    let total = b.chapters.len()
    if finished_by(b, who) {
        "Finished"
    } else if total == 0 {
        ""
    } else {
        "Chapter {(my_chapter(b, who) + 1).clamp(1, total)} of {total}"
    }
}

/// Who wrote a book, as its card says it.
fn author_of(b: Book) -> Text {
    if b.author == "" { "Unknown author" } else { b.author }
}

/// The face the page is set in, as `ui.Reading` names it.
fn face_of(lib: Library) -> Text {
    lib.face
}

/// A highlight colour, by the name it is chosen by and the colour it paints.
type Highlight {
    name: Text
    value: Text
}

/// The colours a passage is highlighted in.
fn highlight_colours() -> List<Highlight> {
    [
        Highlight(name: "Yellow", value: "#f5d76e"),
        Highlight(name: "Green", value: "#b7e1a1"),
        Highlight(name: "Pink", value: "#f5b7d0"),
        Highlight(name: "Blue", value: "#a9cdf5"),
        Highlight(name: "Purple", value: "#d4b7f5"),
    ]
}

/// Which place of the settings strip the library shows: its shelf, its settings, or the books
/// an online search found.
view library(lib: Library) {
    state showing: Int = 0
    let found = lib.found
    ui.NavShell(content_kind: .grid, title: "Books") {
        actions: {
            ui.Button("Add a book", icon: .file_plus, role: .primary) {
                on press: file.choose(open_book(lib), accept: ["application/epub+zip"])
            }
            ui.MenuButton("Sort", icon: .arrow_down_wide_narrow) {
                ui.MenuItem("By title", checked: lib.sort_by == "title") {
                    on choose: set_shelf_sort(lib, "title")
                }
                ui.MenuItem("By author", checked: lib.sort_by == "author") {
                    on choose: set_shelf_sort(lib, "author")
                }
                ui.MenuItem("Added last", checked: lib.sort_by == "added") {
                    on choose: set_shelf_sort(lib, "added")
                }
            }
            ui.Button("Hide finished", icon: .eye_off, toggled: lib.hide_finished) {
                on press: set_hide_finished(lib, not lib.hide_finished)
            }
            ui.ToggleGroup(selected: showing, label: "What the library shows") {
                ui.Segment("Shelf")
                ui.Segment("Settings")
                ui.Segment("Found online")
                on change(i): set(showing, i)
            }
        }
        content: {
            if showing == 1 {
                library_settings(lib)
            } else if showing == 2 {
                found_books(lib, found)
            } else if shelf(lib, invoker).is_empty() {
                ui.EmptyState(
                    "No books yet",
                    icon: .file_text,
                    message: "Open an EPUB file and it stands here, keeping your place as you read.",
                )
            } else {
                ui.Scroll {
                    page(full) {
                        ui.MediaGrid(size: .regular) {
                            for b in shelf(lib, invoker) {
                                ui.MediaCard(
                                    item: b,
                                    title: b.title,
                                    picture: b.cover,
                                    icon: .file_text,
                                    meta: [author_of(b), progress_of(b, invoker)],
                                    aspect: .portrait,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/// How the page is set: its size, spacing, face, how it is shown, how its paragraphs fit, and
/// the palettes it can be set in.
view library_settings(lib: Library) {
    // The face a person is renaming, and what they typed: one face's row is edited at a time.
    state renaming: Text = ""
    state draft: Text = ""
    ui.Scroll {
        page {
            stack(gap: .group) {
                text("Reading settings", role: .title)
                ui.Section("Text") {
                    stack(gap: .related) {
                        ui.Slider(
                            bind: lib.text_size,
                            label: "Text size",
                            min: 0.8,
                            max: 2.0,
                            step: 0.1,
                            shows_value: true,
                        )
                        ui.Slider(
                            bind: lib.line_spacing,
                            label: "Line spacing",
                            min: 0.95,
                            max: 1.6,
                            step: 0.05,
                            shows_value: true,
                        )
                        ui.Toggle(bind: lib.justified, label: "Justified paragraphs")
                    }
                }
                ui.Section("Reading faces") {
                    stack(gap: .related) {
                        for f in lib.fonts.sort_by(.family) {
                            face_row(lib, f, renaming, draft)
                        }
                        ui.MenuButton("Add a face", icon: .plus) {
                            for family in fonts() {
                                ui.MenuItem(font_family_title(family), sample: family) {
                                    on choose: add_font(lib, family)
                                }
                            }
                        }
                    }
                }
                ui.Section("Watched folders") {
                    stack(gap: .related) {
                        for folder in lib.folders.sort_by(.path) {
                            ui.ListRow(
                                title: folder.path,
                                secondary: "New EPUBs here become books",
                                icon: .folder,
                            )
                        }
                        row(gap: .related, align: .center) {
                            ui.TextInput(
                                bind: lib.draft_folder,
                                placeholder: "A folder on this device",
                                fill,
                            )
                            ui.Button("Watch it", icon: .folder_open) {
                                on press: watch_folder(lib, lib.draft_folder)
                            }
                        }
                    }
                }
                ui.Section("Page palettes") {
                    stack(gap: .related) {
                        for s in lib.schemes.sort_by(.name) {
                            ui.ListRow(
                                title: s.name,
                                secondary: "Paper {s.page}, ink {s.ink}",
                                icon: .palette,
                                selected: s.name == lib.scheme,
                            ) {
                                on activate: set_scheme(lib, s.name)
                                trailing: {
                                    if not s.builtin {
                                        ui.Button(
                                            "Delete {s.name}",
                                            icon: .trash_2,
                                            icon_only: true,
                                        ) {
                                            on press: delete_scheme(lib, s)
                                        }
                                    }
                                }
                            }
                        }
                        stack(gap: .related) {
                            ui.TextInput(bind: lib.draft_scheme, placeholder: "Palette name")
                            row(gap: .related, align: .center) {
                                ui.ColorPicker(bind: lib.custom_page, label: "Paper")
                                ui.ColorPicker(bind: lib.custom_ink, label: "Ink")
                                ui.Button("Use these colours", icon: .palette) {
                                    on press: add_scheme(
                                            lib,
                                            lib.draft_scheme.trim(),
                                            lib.custom_page,
                                            lib.custom_ink,
                                        )
                                        .then(
                                            set_scheme(lib, lib.draft_scheme.trim()),
                                        )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One face the page can be set in: its row, and, for the face the page is set in, what it is
/// called, the family it is, whether the picker offers it, how large it reads, and the way to
/// rename it or take it away. A face that is not the page's is its row alone, so every face the
/// reader knows stands in the list at the height a window shows.
view face_row(lib: Library, f: Font, renaming: State<Text>, draft: State<Text>) {
    let chosen = lib.face == f.family
    stack(gap: .tight) {
        ui.ListRow(
            item: f,
            title: font_title(f),
            secondary: "{f.family} · set at {f.size}×",
            icon: .type,
            selected: chosen,
        ) {
            on activate: set_face(lib, f.family)
            trailing: {
                ui.Button(
                    if f.shown { "Hide" } else { "Show" },
                    icon: if f.shown { .eye_off } else { .eye },
                    size: .small,
                ) {
                    on press: set_font_shown(lib, f, not f.shown)
                }
            }
        }
        if chosen {
            ui.Slider(
                bind: f.size,
                label: "How large {font_title(f)} reads",
                min: 0.7,
                max: 1.4,
                step: 0.05,
                shows_value: true,
            )
            row(gap: .related, align: .center) {
                ui.Button("Rename", icon: .pencil, size: .small) {
                    on press: set(draft, f.name).then(set(renaming, f.family))
                }
                if not f.builtin {
                    ui.Button("Remove", icon: .trash_2, size: .small) {
                        on press: remove_font(lib, f)
                    }
                }
            }
            if renaming == f.family {
                row(gap: .related, align: .center) {
                    ui.TextInput(bind: draft, placeholder: "The name it shows as")
                    ui.Button("Save name", icon: .check) {
                        on press: rename_font(lib, f, draft).then(set(renaming, ""))
                    }
                    ui.Button("Cancel", icon: .x) {
                        on press: set(renaming, "")
                    }
                }
            }
        }
    }
}

/// The books an online search found, each with the way to download it.
view found_books(lib: Library, found: Collection<Found>) {
    state query: Text = ""
    // The book a download made opens as its own page: the window's answer to what the press
    // made.
    on show(b: Book): open(b)
    ui.Scroll {
        page {
            stack(gap: .group) {
                text("Find a book online", role: .title)
                row(gap: .related, align: .center) {
                    ui.TextInput(bind: query, placeholder: "Title, author or subject", fill)
                    ui.Button("Search", icon: .search) {
                        on press: find_books(lib, query)
                    }
                    ui.Button("Clear", icon: .trash_2) {
                        on press: clear_found(lib)
                    }
                }
                if found.is_empty() {
                    ui.EmptyState(
                        "Nothing found yet",
                        icon: .search,
                        message: "Search Project Gutenberg for a free book; it stands here until you download it.",
                        inline: true,
                    )
                } else {
                    for f in found.sort_by(.title) {
                        ui.ListRow(
                            item: f,
                            title: f.title,
                            secondary: f.author,
                            icon: .cloud_download,
                        ) {
                            on activate: download_found(lib, f)
                        }
                    }
                }
            }
        }
    }
}

/// The line over the open chapter while the window's search holds words: how many passages of
/// the chapter have them, and the way from one to the next.
template view find_line(b: Book, chapter: Int, find: Text, hit: State<Int>, reveal: State<Int>) {
    let hits = hits_in(b, chapter, find)
    row(gap: .related, align: .center, padding: .related) {
        text(matches_said(hits.len()), role: .caption, secondary: true)
        spacer()
        ui.Button("Previous passage", icon: .arrow_up, size: .small) {
            on press: set(hit, if hit > 0 { hit - 1 } else { 0 })
                .then(set(reveal, hit_at(hits_in(b, chapter, find), hit - 1)))
        }
        ui.Button("Next passage", icon: .arrow_down, size: .small) {
            on press: set(hit, hit + 1)
                .then(set(reveal, hit_at(hits_in(b, chapter, find), hit + 1)))
        }
    }
}

/// A book's page: its contents and marks down the side, the open chapter set a page at a time,
/// the controls a reader needs, and what a selected passage can become.
view book_page(b: Book) {
    state chapter: Int = my_chapter(b, invoker)
    state page: Int = my_page(b, invoker)
    state place: Float = my_place(b, invoker)
    state reveal: Int = my_block(b, invoker)
    state pages: Int = 1
    // How this window shows the page: a page at a time, one column scrolled, or two pages
    // side by side. What a window shows is the window's, not the library's.
    state mode: Text = "paged"
    // The words the window's search holds, and which of the passages they were found in the
    // person stands at.
    state find: Text = ""
    state hit: Int = 0
    // The passage selected: each range's block, first and last character, in threes.
    state picked: List<Int> = []
    state noting: Bool = false
    state note_text: RichText = RichText("")
    state commenting: Option<Mark> = .none
    state comment_text: RichText = RichText("")
    // What the book's own details are edited to before they are saved.
    state details: Bool = false
    state edit_title: Text = b.title
    state edit_author: Text = b.author
    state edit_subject: Text = b.subject
    state edit_language: Text = b.language
    let lib = root Library
    let open_chapter = b.chapters.at(chapter)
    let chapter_marks = b.marks.filter(m -> m.chapter == chapter)
    let bookmarks = b.marks.filter(m -> m.kind == "bookmark")
    let notes = b.marks.filter(m -> m.kind == "note" or m.kind == "comment")
    ui.NavShell(content_kind: .reading, title: b.title, sidebar_title: "Contents") {
        // The window's search is the book's find: the passages of the open chapter that hold
        // the words stand counted above it, the first of them shown.
        on search(words: Text): set(find, words)
            .then(set(hit, 0))
            .then(set(reveal, first_hit(b, chapter, words)))
        actions: {
            // Search inside this book: the same search, narrowed to the chapters it holds, so
            // a passage found opens here at it.
            ui.SearchButton(item: b, label: b.title)
            ui.Button("Justified", icon: .align_justify, icon_only: true, toggled: lib.justified) {
                on press: set_justified(lib, not lib.justified)
            }
            ui.Button("Book details", icon: .pencil, icon_only: true) {
                on press: set(details, true)
            }
            ui.Button("Reading settings", icon: .settings, icon_only: true) {
                on press: open(lib)
            }
        }
        sidebar: {
            ui.Scroll {
                stack(gap: .group) {
                    ui.NavList(selected: "{chapter}") {
                        for c in b.chapters {
                            ui.NavItem("{b.chapters.index_of(c) ?? 0}", c.title, item: c) {
                                on select: read_chapter(b, invoker, c)
                                    .then(
                                        set(chapter, b.chapters.index_of(c) ?? 0),
                                    )
                                    .then(set(page, 0))
                                    .then(set(reveal, -1))
                            }
                        }
                    }
                    if not bookmarks.is_empty() {
                        ui.Section("Bookmarks") {
                            for m in bookmarks.sort_by(.chapter) {
                                ui.ListRow(
                                    item: m,
                                    title: m.excerpt,
                                    secondary: "Chapter {m.chapter + 1}",
                                    icon: .bookmark,
                                ) {
                                    on activate: set(chapter, m.chapter)
                                        .then(
                                            set(reveal, m.block),
                                        )
                                        .then(
                                            show_passage(b, invoker, m.chapter, m.block),
                                        )
                                }
                            }
                        }
                    }
                    if not notes.is_empty() {
                        ui.Section("Notes") {
                            for m in notes.sort_by(.chapter) {
                                ui.ListRow(
                                    item: m,
                                    title: m.excerpt,
                                    secondary: m.text.text,
                                    icon: .pencil,
                                ) {
                                    on activate: set(chapter, m.chapter)
                                        .then(
                                            set(reveal, m.block),
                                        )
                                        .then(
                                            show_passage(b, invoker, m.chapter, m.block),
                                        )
                                }
                            }
                        }
                    }
                }
            }
        }
        content: {
            if let open = open_chapter {
                stack(fill, gap: .related) {
                    if find.trim() != "" {
                        find_line(b, chapter, find, hit, reveal)
                    }
                    ui.Reading(
                        value: open.body,
                        scale: reading_scale(lib),
                        spacing: lib.line_spacing,
                        justified: lib.justified,
                        face: face_of(lib),
                        language: b.language,
                        mode: mode,
                        page: page,
                        place: place,
                        reveal: if reveal >= 0 { .some(reveal) } else { .none },
                        paper: paper_of(lib),
                        ink: ink_of(lib),
                        highlights: chapter_marks
                            .filter(
                                m -> m.kind == "highlight" or m.kind == "note",
                            )
                            .flat_map(m -> bands_of(m)),
                        fill,
                    ) {
                        on turned(p: Int, top: Int): record_at(b, invoker, chapter, top, p, 0.0)
                            .then(set(page, p))
                        on pages(n: Int): set(pages, n)
                        on moved(p: Float): show_place(
                                b,
                                invoker,
                                chapter,
                                my_block(b, invoker),
                                page,
                                p,
                            )
                            .then(set(place, p))
                        on showing(seeing: List<Int>): saw_blocks(b, invoker, seeing)
                        on selected(at: List<Int>): set(picked, at).then(set(noting, false))
                    }
                    if not picked.is_empty() {
                        row(gap: .related, align: .center) {
                            text(passage_words(b, chapter, picked), role: .caption, secondary: true)
                            ui.MenuButton("Highlight", icon: .highlighter) {
                                for colour in highlight_colours() {
                                    ui.MenuItem(colour.name) {
                                        on choose: highlight(b, chapter, picked, colour.value)
                                            .then(
                                                set(picked, []),
                                            )
                                    }
                                }
                            }
                            ui.Button("Copy", icon: .copy) {
                                on press: copy_passage(passage_words(b, chapter, picked))
                            }
                            ui.Button("Note", icon: .pencil) {
                                on press: set(noting, true)
                            }
                            ui.Button("Bookmark", icon: .bookmark) {
                                on press: toggle_bookmark(b, chapter, picked.at(0) ?? 0)
                                    .then(set(picked, []))
                            }
                            ui.Button("Dismiss", icon: .x) {
                                on press: set(picked, [])
                            }
                        }
                    }
                    if noting {
                        row(gap: .related, align: .center) {
                            rich_input(bind: note_text, placeholder: "Write a note on the passage")
                            ui.Button("Save note") {
                                on press: add_note(b, chapter, picked, "#f5d76e", note_text)
                                    .then(set(noting, false))
                                    .then(
                                        set(note_text, RichText("")),
                                    )
                                    .then(set(picked, []))
                            }
                        }
                    }
                    row(gap: .related, align: .center) {
                        if mode != "continuous" {
                            ui.Button("Previous", icon: .chevron_left) {
                                on press: set(page, (page - 1).max(0))
                            }
                            text("Page {page + 1} of {pages}", role: .caption)
                            ui.Button("Next", icon: .chevron_right) {
                                on press: set(page, (page + 1).min(pages - 1))
                            }
                        }
                        spacer()
                        ui.Button("Smaller text", icon: .a_large_small, icon_only: true) {
                            on press: set_text_size(lib, lib.text_size - 0.1)
                        }
                        ui.Button("Larger text", icon: .a_arrow_up, icon_only: true) {
                            on press: set_text_size(lib, lib.text_size + 0.1)
                        }
                        ui.Button("Change the page palette", icon: .palette, icon_only: true) {
                            on press: cycle_scheme(lib)
                        }
                        ui.MenuButton("Face", icon: .type, icon_only: true) {
                            for f in shown_fonts(lib) {
                                ui.MenuItem(
                                    font_title(f),
                                    checked: lib.face == f.family,
                                    sample: f.family,
                                ) {
                                    on choose: set_face(lib, f.family)
                                }
                            }
                        }
                        if mode == "continuous" {
                            ui.Button("Pages", icon: .columns_2, icon_only: true) {
                                on press: set(mode, "paged")
                            }
                        } else if mode == "two_up" {
                            ui.Button("One page", icon: .file_text, icon_only: true) {
                                on press: set(mode, "paged")
                            }
                        } else {
                            ui.Button("Two pages", icon: .columns_2, icon_only: true) {
                                on press: set(mode, "two_up")
                            }
                        }
                        if mode != "continuous" {
                            ui.Button("Scrolled", icon: .list, icon_only: true) {
                                on press: set(mode, "continuous")
                            }
                        }
                        ui.Button("Bookmark this page", icon: .bookmark, icon_only: true) {
                            on press: toggle_bookmark(b, chapter, my_block(b, invoker))
                        }
                        ui.Button(
                            "Finished",
                            icon: .circle_check,
                            icon_only: true,
                            toggled: finished_by(b, invoker),
                        ) {
                            on press: set_finished(b, invoker, not finished_by(b, invoker))
                        }
                    }
                    ui.ProgressBar(
                        value: (page + 1).to_float() / pages.max(1).to_float(),
                        label: "How far through this chapter",
                    )
                    if not chapter_marks
                        .filter(
                            m -> m.kind == "highlight" or m.kind == "note",
                        )
                        .is_empty() {
                        ui.Section("This chapter's marks") {
                            for m in chapter_marks.filter(
                                x -> x.kind == "highlight" or x.kind == "note",
                            ) {
                                mark_line(b, m, commenting, comment_text)
                            }
                        }
                    }
                }
            }
        }
    }
    ui.ModalDialog(
        title: "Book details",
        open: details,
        primary: "Save",
        secondary: "Cancel",
        primary_enabled: edit_title.trim() != "",
    ) {
        body: {
            stack(gap: .related) {
                ui.TextInput(bind: edit_title, placeholder: "Title", autofocus: true)
                ui.TextInput(bind: edit_author, placeholder: "Author")
                ui.TextInput(bind: edit_subject, placeholder: "Subject")
                ui.TextInput(bind: edit_language, placeholder: "Language, as `en` or `de`")
                row(gap: .related, align: .center) {
                    ui.Button("Choose a cover", icon: .image) {
                        on press: file.choose(set_cover(b), accept: ["image/*"])
                    }
                    if b.cover != "" {
                        ui.Button("Take the cover off", icon: .trash_2) {
                            on press: clear_cover(b)
                        }
                    }
                }
            }
        }
        on act_primary: set_title(b, edit_title)
            .then(set_author(b, edit_author))
            .then(set_subject(b, edit_subject))
            .then(set_language(b, edit_language))
            .then(set(details, false))
        on act_secondary: set(details, false)
        on dismiss: set(details, false)
    }
}

/// One mark of the open chapter, with its note and the comments on it, and the way to comment
/// on it, recolour it or delete it.
view mark_line(b: Book, m: Mark, commenting: State<Option<Mark>>, comment_text: State<RichText>) {
    state editing: Bool = false
    state draft: RichText = m.text
    stack(gap: .tight) {
        ui.ListRow(
            item: m,
            title: m.excerpt,
            secondary: if m.text.text == "" { m.kind } else { m.text.text },
            icon: if m.kind == "note" { .pencil } else { .highlighter },
        ) {
            on activate: show_passage(b, invoker, m.chapter, m.block)
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Comment", icon: .message_square) {
                on press: set(commenting, m)
            }
            if m.kind == "note" or m.kind == "comment" {
                ui.Button("Edit", icon: .pencil) {
                    on press: set(draft, m.text).then(set(editing, true))
                }
            }
            for colour in highlight_colours() {
                ui.Button(colour.name, size: .small) {
                    on press: change_color(b, m, colour.value)
                }
            }
            ui.Button("Delete", icon: .trash_2) {
                on press: delete_mark(b, m)
            }
        }
        if editing {
            row(gap: .related, align: .center) {
                rich_input(bind: draft, placeholder: "Edit the note")
                ui.Button("Save", icon: .check) {
                    on press: edit_mark_text(b, m, draft).then(set(editing, false))
                }
                ui.Button("Cancel", icon: .x) {
                    on press: set(editing, false)
                }
            }
        }
        for c in comments_on(b, m) {
            ui.ListRow(title: c.text.text, secondary: "Comment", icon: .message_square)
        }
        if let on = commenting {
            if on == m {
                row(gap: .related, align: .center) {
                    rich_input(bind: comment_text, placeholder: "Comment on the highlight")
                    ui.Button("Send comment") {
                        on press: comment(b, m, comment_text)
                            .then(set(commenting, .none))
                            .then(set(comment_text, RichText("")))
                    }
                }
            }
        }
    }
}

/// A chapter's own page, for a search that found a passage in it: its words, and the way into
/// the book it belongs to.
view chapter_page(c: Chapter) {
    let lib = root Library
    ui.NavShell(content_kind: .reading, title: c.title) {
        actions: {
            if let b = c.book {
                ui.Button("Read in {b.title}", icon: .file_text) {
                    on press: read_chapter(b, invoker, c).then(open(b))
                }
            }
        }
        content: {
            ui.Scroll {
                page {
                    ui.Reading(
                        value: c.body,
                        scale: reading_scale(lib),
                        spacing: lib.line_spacing,
                        justified: lib.justified,
                        face: face_of(lib),
                        mode: "continuous",
                        paper: paper_of(lib),
                        ink: ink_of(lib),
                        fill,
                    )
                }
            }
        }
    }
}

/// A mark's own page: the passage, what it says, the comments on it, and the way into the book.
view mark_page(m: Mark) {
    let lib = root Library
    ui.Scroll {
        page {
            stack(gap: .group) {
                text(m.excerpt, role: .title)
                if m.kind == "bookmark" {
                    text("A bookmark", role: .caption, secondary: true)
                } else {
                    text(m.text.text, role: .reading)
                }
                text("Chapter {m.chapter + 1}", role: .caption, secondary: true)
                if let b = m.book {
                    ui.Button("Show it in {b.title}", icon: .file_text) {
                        on press: show_passage(b, invoker, m.chapter, m.block).then(open(b))
                    }
                }
            }
        }
    }
}

/// A book as a row: what it is, who wrote it, and how far the person is.
view book_row(b: Book) {
    ui.ListRow(
        item: b,
        title: b.title,
        secondary: author_of(b),
        icon: .file_text,
        value: progress_of(b, invoker),
    )
}

/// A mark as a row: the passage, and what it says.
view mark_row(m: Mark) {
    ui.ListRow(
        item: m,
        title: m.excerpt,
        secondary: if m.text.text == "" { m.kind } else { m.text.text },
        icon: if m.kind == "bookmark" {
            .bookmark
        } else if m.kind == "highlight" { .highlighter } else { .pencil },
    )
}
