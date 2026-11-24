app so.tilo.pdf-viewer "PDF Viewer" (
    icon: .file,
    category: .office,
    tagline: "Read, mark up and sign PDFs",
    keywords: ["pdf", "reader", "document", "annotate", "highlight", "sign", "form", "dokument", "lesen", "kommentieren", "markieren", "unterschreiben", "formular", "pdf-betrachter"],
    document: library.documents,
)

/// A PDF the workspace reads: the pages it shows, and every mark, comment and signature its
/// readers left on it. The pages come from the address of the file's bytes in the workspace's
/// content, so every member reads the same PDF and works on the same marks.
type Document (title: source_name, untitled: "Untitled document") {
    /// The address of the PDF's bytes, `tilo-blob://…`.
    source: Text
    /// What the file is called.
    source_name: Text
    /// Why the pages cannot be shown, in the engine's words; empty while they can.
    trouble: Text = ""
    /// The source whose own mark-up was taken over, once.
    imported_source: Text = ""
    /// Why a copy could not be saved, in the engine's words; empty until one is tried.
    save_trouble: Text = ""
    pages: Collection<Page>
    /// A page drawn at one width, by the address of the picture: what the reader shows.
    pictures: Collection<Picture>
    /// The document's contents, as the engine reads them.
    outlines: Collection<Outline>
    /// The links its pages carry, as the file names them.
    links: Collection<Link>
    /// The form fields the file carries, where they stand and what they hold.
    fields: Collection<Field>
    marks: Collection<Mark>
    comments: Collection<Comment>
    signatures: Collection<Signature>
    /// What people filled into the form, by field name.
    form_text: Map<Text>
    form_choice: Map<Text>
    form_checked: Map<Bool>
    form_export: Map<Text>
    /// The mark-up the file itself carried, by the id each mark of it was given: the fingerprint
    /// it had when it was taken over, so a save keeps an unchanged one as the file's own object.
    /// One entry reads `page@index@name@kind@fingerprint`.
    adopted: Map<Text>
    /// The page the reader was on, how far down it, and the zoom in percent.
    reading_page: Option<Page>
    reading_fraction: Float = 0.0
    reading_zoom: Float = 0.0

    search {
        sort source_name a_z
    }
}

/// One page of a document: which page of which bytes it shows, how big it is and how far it is
/// turned. Its words are read once, so the workspace's search finds the page by them.
type Page (title: words) {
    /// The page of the file it comes from, counted from 1.
    source_page: Int
    /// The address of the bytes of an imported page; empty for the document's own file.
    imported: Text = ""
    /// What those bytes are, `pdf` or `image`; empty for the document's own file.
    kind: Text = ""
    /// Its width and height as the file draws it, in points.
    width: Float
    height: Float
    /// A quarter turn added to the page's own.
    rotation: Int = 0
    /// Its words, as the engine reads them; empty until they are read.
    words: Text = ""
    /// Its words as a person selects them, one run of a line at a time: each run's box in
    /// fractions of the page's box (x, y, width, height, four in turn, in reading order), and
    /// its words. A selection takes a run whole, so what a person copies is what the page says.
    runs: List<Float>
    run_words: List<Text>
    /// Whether its words were read: a page of pictures has been read too, and holds none.
    read: Bool = false

    search {
        hide source_page
        hide imported
        hide kind
        hide width
        hide height
        hide rotation
        hide read
        hide runs
        hide run_words
    }
}

/// A page drawn once at one width, kept as the address of its picture: redrawing every page on
/// every look costs the engine a render each, and a page is the same picture until its turn,
/// its source or the width asked for changes.
type Picture (untitled: "Page picture") {
    page: Page
    /// The width it was drawn at, in pixels.
    width: Int
    url: Text
}

/// One entry of the document's contents, as the engine lists it, flattened.
type Outline (title: title) {
    title: Text
    /// The page it leads to, counted from 1; 0 for an entry that leads nowhere.
    page: Int
    /// How deep it stands: 0 at the top.
    level: Int
}

/// A link a page carries: the area it covers and where it leads.
type Link (untitled: "Link") {
    page: Page
    /// Its area in points of the page as the file draws it: x, y, width, height.
    rect: List<Float>
    /// What it leads to: a page of this document, an address, or a file.
    kind: Text
    /// The page it leads to, counted from 1, or the address or path it names.
    to: Text
}

/// One form field the file carries, where its widget stands on a page and what it holds.
type Field (title: name) {
    name: Text
    /// `text`, `checkbox`, `radio` or `choice`.
    kind: Text
    value: Text = ""
    checked: Bool = false
    export: Text = ""
    options: List<Text>
    /// The page its widget stands on, counted from 1; 0 for one with no widget.
    page: Int = 0
    /// Its widget's area in points: x, y, width, height.
    rect: List<Float>
    read_only: Bool = false
    required: Bool = false
}

/// What a mark does on a page.
enum MarkKind {
    /// Text marked with a highlighter, an underline, a strikethrough or a squiggle.
    markup
    /// Free-hand ink.
    ink
    /// A sticky note.
    note
    /// A box of words.
    text
}

/// One mark a reader left on a page.
type Mark (untitled: "Mark") {
    page: Page
    kind: MarkKind
    /// The colour it is drawn in, `#rrggbb`.
    colour: Text
    /// What a text mark draws: `highlight`, `underline`, `strikeout` or `squiggly`.
    style: Text = "highlight"
    /// The rectangles it covers, `x, y, width, height` one after another, in points of the page
    /// as the file draws it: a text mark's lines, a text box's box.
    rects: List<Float>
    /// Where a sticky note stands, in points.
    x: Float = 0.0
    y: Float = 0.0
    /// A text box's or a note's words.
    words: Text = ""
    /// A text box's letters, in points.
    size: Float = 14.0
    /// The strokes ink or a drawn signature was made of, as points x and y in turn in fractions
    /// of the page's box.
    strokes: Collection<Stroke>
    /// Who left it, when the file says; empty for a mark made here, whose author is its change.
    author: Text = ""
    /// The file's own mark this was taken over from, by the id the engine names it; empty for a
    /// mark made here. A save finds the file's object by it, so an unchanged one is kept.
    adopted_id: Text = ""
}

/// One stroke of a drawing: its points, x and y in turn.
type Stroke {
    points: List<Float>
}

/// What people said about a page, or about one of its marks.
type Comment (title: words) {
    /// The mark it is about; none for one about the page itself.
    mark: Option<Mark>
    page: Page
    words: Text
    body: RichText
    resolved: Bool = false
    replies: Collection<Reply>

    search {
        filter "Open" = not resolved (icon: .message_circle)
        filter "Resolved" = resolved (icon: .check)
    }
}

/// One answer in a comment's conversation.
type Reply (title: words) {
    words: Text
    body: RichText
}

/// A signature placed on a page.
type Signature (title: signer) {
    page: Page
    x: Float
    y: Float
    width: Float
    height: Float
    signer: Text
    /// The day it was signed, as the signer wrote it.
    date: Text
    /// The strokes of a drawn signature, in fractions of its box; none for a typed one, which
    /// is the signer's name set in the box.
    strokes: Collection<Stroke>
}

/// The workspace's PDFs, and the signature a person reuses on every document they sign.
type Library {
    documents: Collection<Document>
    /// The name a typed signature carries; empty until a person sets one.
    signer: Text = ""
    /// The strokes of a drawn signature, so the next document is signed with one press; empty
    /// until a person draws one.
    strokes: Collection<Stroke>
}

root library: Library

default view library for Library

default view reader for Document

row view document_row for Document

/// A PDF opens in the reader: Files, the person's own file system and the system's search hand
/// it over as the file it is, and it opens on the document those bytes already are.
primary action open_pdf(lib: Library, file: File) -> Document (
    icon: .file_plus,
    label: "Open PDF",
    opens: ["application/pdf", .pdf],
    did: "opened a PDF",
) {
    open_source(lib, file.url, file.name)
}

/// Opens the PDF whose bytes the workspace keeps at `address`: the document those bytes are if
/// one holds them, else a new one. How an agent, a link or another app opens a PDF.
action open_source(lib: Library, address: Text, name: Text = "Document.pdf") -> Document (
    icon: .file_plus,
    label: "Open a PDF from the workspace's content",
    did: "opened a PDF",
) {
    require address != "" "A document is opened from the address of its bytes"
    if let known = lib.documents.find(d -> d.source == address) {
        return known
    }
    let made = lib.documents.insert(
        Document(source: address, source_name: if name == "" { "Document.pdf" } else { name }),
    )
    lay_out(made)
    made
}

/// Lays a document out on its own bytes: one page for each page of the file, in their order, at
/// the size the file draws it, its contents and its form.
action lay_out(d: Document) (label: "Lay out the pages", did: "laid out the pages") {
    if not d.pages.is_empty() {
        return
    }
    match pdf.pages(source_of(d)) {
        .ok(text) => match json.decode<List<PageRow>>(text) {
            .ok(rows) => {
                for row in rows {
                    d.pages.insert(
                        Page(source_page: row.page, width: row.width, height: row.height),
                    )
                }
                d.trouble = ""
                read_outline(d)
                read_fields(d)
                // A file that came with its own highlights, notes and comments shows them as
                // the document's, so a person reading it sees what was already marked.
                adopt_markup(d)
            }
            .err(why) => {
                d.trouble = why
            }
        },
        .err(why) => {
            d.trouble = why
        }
    }
}

/// Reads the document's contents, so the review panel lists them without asking the engine
/// each time it is shown.
action read_outline(d: Document) (label: "Read the contents") {
    match pdf.outline(source_of(d)) {
        .ok(text) => match json.decode<List<OutlineRow>>(text) {
            .ok(rows) => {
                for row in rows {
                    if not d.outlines.any(
                        x -> x.title == row.title and x.page == row.page and x.level == row.level,
                    ) {
                        d.outlines.insert(
                            Outline(title: row.title, page: row.page, level: row.level),
                        )
                    }
                }
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// Reads the document's form fields, so their widgets are drawn over the pages and filled in
/// place.
action read_fields(d: Document) (label: "Read the form") {
    match pdf.fields(source_of(d)) {
        .ok(text) => match json.decode<List<FieldRow>>(text) {
            .ok(rows) => {
                for row in rows {
                    if not d.fields.any(x -> x.name == row.name) {
                        d.fields.insert(
                            Field(
                                name: row.name,
                                kind: row.kind,
                                value: row.value,
                                checked: row.checked,
                                export: row.export,
                                options: row.options,
                                page: row.page ?? 0,
                                rect: row.rect ?? [],
                                read_only: row.read_only,
                                required: row.required,
                            ),
                        )
                    }
                }
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// Reads the page's words once, so the workspace's search finds the page by them and a person
/// can select them, and reads the links it carries.
action read_page(d: Document, p: Page) (label: "Read a page's words") {
    if p.read {
        return
    }
    match pdf.text(file_of(d, p), p.source_page) {
        .ok(text) => match json.decode<List<GlyphRow>>(text) {
            .ok(rows) => {
                p.words = words_of(rows)
                let runs = runs_of(rows, p)
                p.runs = runs.rects
                p.run_words = runs.texts
                read_links(d, p)
                p.read = true
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// Reads the links the page carries: each area it covers and where it leads.
action read_links(d: Document, p: Page) (label: "Read a page's links") {
    match pdf.links(file_of(d, p), p.source_page) {
        .ok(text) => match json.parse(text) {
            .ok(document) => match document {
                .array(rows) => {
                    for row in rows {
                        let rect = [
                            number_in(row, "rect", "x"),
                            number_in(row, "rect", "y"),
                            number_in(row, "rect", "w"),
                            number_in(row, "rect", "h"),
                        ]
                        let kind = text_of(field_of(row, "kind"))
                        let leads = link_text(field_of(row, "to"))
                        if not d.links.any(
                            l -> l.page == p
                                and l.rect == rect
                                and l.kind == kind
                                and l.to == leads,
                        ) {
                            d.links.insert(Link(page: p, rect: rect, kind: kind, to: leads))
                        }
                    }
                }
                .object(fields) => {}
                .string(text) => {}
                .number(number) => {}
                .bool(flag) => {}
                .null => {}
            },
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// A page's words as runs a person selects: each run's box and its words, the boxes in
/// fractions of the page's box, in reading order.
type Runs {
    rects: List<Float>
    texts: List<Text>
}

/// The runs `rows` make: a run per word, a line at a time, its box the words' own.
fn runs_of(rows: List<GlyphRow>, p: Page) -> Runs {
    let runs = Runs(rects: [], texts: [])
    let word = ""
    let x1 = 0.0
    let y1 = 0.0
    let x2 = 0.0
    let y2 = 0.0
    let line = -1
    for row in rows {
        let blank = row.text.trim() == ""
        if blank or line != -1 and row.line != line {
            if word.trim() != "" {
                runs = add_run(runs, word.trim(), x1, y1, x2, y2, p)
                word = ""
            }
            // The run is finished: the next word starts its own box, wherever it stands, rather
            // than growing out of the last one's first letter.
            line = -1
        }
        if not blank {
            if line != row.line {
                line = row.line
                x1 = row.x
                y1 = row.y
                x2 = row.x + row.w
                y2 = row.y + row.h
            } else {
                x1 = x1.min(row.x)
                y1 = y1.min(row.y)
                x2 = x2.max(row.x + row.w)
                y2 = y2.max(row.y + row.h)
            }
            word = "{word}{row.text}"
        }
    }
    if word.trim() != "" {
        add_run(runs, word.trim(), x1, y1, x2, y2, p)
    } else { runs }
}

/// `runs` with one more run: its words and box in fractions of `p`'s box.
fn add_run(runs: Runs, word: Text, x1: Float, y1: Float, x2: Float, y2: Float, p: Page) -> Runs {
    let w = if p.width > 0.0 { p.width } else { 1.0 }
    let h = if p.height > 0.0 { p.height } else { 1.0 }
    Runs(
        rects: [runs.rects, [x1 / w, y1 / h, (x2 - x1) / w, (y2 - y1) / h]].flat_map(x -> x),
        texts: [runs.texts, [word]].flat_map(x -> x),
    )
}

/// Draws one page as a picture at `width` pixels, kept so a person looking at it again, or
/// another member of the workspace, does not wait for the engine.
action show_page(d: Document, p: Page, width: Int) (icon: .image, label: "Draw a page") {
    let wanted = width.clamp(64, 2048)
    if d.pictures.any(x -> x.page == p and x.width == wanted) {
        return
    }
    match pdf.picture(file_of(d, p), p.source_page, wanted) {
        .ok(file) => {
            if let kept = d.pictures.find(x -> x.page == p) {
                kept.width = wanted
                kept.url = file.url
            } else {
                d.pictures.insert(Picture(page: p, width: wanted, url: file.url))
            }
        }
        .err(why) => {
            d.trouble = why
        }
    }
}

/// Draws the first page that has no picture at the width the reader asks for, every second
/// while one is missing: a long document fills in page by page, without anyone waiting for it.
trigger draw_pages(d: Document) every 1s: draw_next(d)

/// Reads the next page of each document nobody read yet, every second: a long PDF becomes
/// searchable, and its words selectable, as its pages are read, without anyone waiting for the
/// open.
trigger read_pages(d: Document) every 1s: read_next(d)

/// Reads the words of the next page nobody read.
action read_next(d: Document) (label: "Read the next page's words") {
    if let next = d.pages.find(p -> not p.read) { read_page(d, next) }
}

/// Draws the next page nobody drew at the reader's zoom.
action draw_next(d: Document) (label: "Draw the next page") {
    let width = width_for(d)
    if let next = d.pages.find(p -> not d.pictures.any(x -> x.page == p and x.width == width)) {
        show_page(d, next, width)
    }
}

/// The width a page is drawn at, in pixels: what the reading zoom asks for, never more than a
/// page needs.
fn width_for(d: Document) -> Int {
    let zoom = if d.reading_zoom <= 0.0 { 100.0 } else { d.reading_zoom }
    (zoom * 8.0).to_int()
}

/// Turns a page a quarter clockwise.
action rotate_page(d: Document, p: Page) (icon: .rotate_cw, label: "Rotate page") {
    p.rotation = (p.rotation + 90) % 360
}

/// Puts a page after another one, or first.
action move_page(d: Document, p: Page, after: Option<Page>) (label: "Move page", group: "Pages") {
    require after != .some(p) "A page cannot go after itself"
    if let x = after { d.pages.move(p, after: x) } else { d.pages.move(p, to: 0) }
}

/// Copies a page right after itself. The marks stay with the page they were made on.
action duplicate_page(d: Document, p: Page) -> Page (icon: .copy, label: "Duplicate page") {
    let copy = d.pages.insert(
        Page(
            source_page: p.source_page,
            imported: p.imported,
            kind: p.kind,
            width: p.width,
            height: p.height,
            rotation: p.rotation,
        ),
    )
    d.pages.move(copy, after: p)
    copy
}

/// Takes a page out of the document; a document keeps at least one.
destructive action delete_page(d: Document, p: Page) (icon: .trash, label: "Delete page") {
    require d.pages.len() > 1 "A document keeps at least one page"
    for m in d.marks.filter(m -> m.page == p) {
        d.marks.remove(m)
    }
    for picture in d.pictures.filter(x -> x.page == p) {
        d.pictures.remove(picture)
    }
    d.pages.remove(p)
}

/// Takes the pages of another PDF or of a picture into the document, after a page (first when
/// none is named).
action import_pages(d: Document, after: Option<Page>, file: File) (
    icon: .file_plus,
    label: "Import pages",
    did: "imported pages",
) {
    match pdf.import(file) {
        .ok(text) => match json.decode<ImportRow>(text) {
            .ok(held) => {
                let at = after
                for row in held.pages {
                    let page = d.pages.insert(
                        Page(
                            source_page: row.page,
                            imported: file.url,
                            kind: held.kind,
                            width: row.width,
                            height: row.height,
                        ),
                    )
                    if let x = at {
                        d.pages.move(page, after: x)
                    } else { d.pages.move(page, to: 0) }
                    at = .some(page)
                }
            }
            .err(why) => {
                d.trouble = why
            }
        },
        .err(why) => {
            d.trouble = why
        }
    }
}

/// Marks text on a page: a highlight, an underline, a strikethrough or a squiggle over the lines
/// a selection covers, in the reader's colour.
action mark_text(d: Document, p: Page, style: Text, colour: Text, rects: List<Float>) -> Mark (
    icon: .highlighter,
    label: "Mark text",
    did: "marked text",
) {
    require not rects.is_empty() "A mark covers something"
    d.marks.insert(Mark(page: p, kind: .markup, style: style, colour: colour, rects: rects))
}

/// Marks exactly the words a selection covers: the runs of `p` from `first` to `last`, both
/// counted in, as a highlight, an underline, a strikethrough or a squiggle in the colour the
/// reader holds. This is how a person marks lines of text they selected, the way the words are
/// read; dragging a box still marks the words the box covers.
action mark_selection(
    d: Document,
    p: Page,
    style: Text,
    colour: Text,
    first: Int,
    last: Int,
) -> Mark (icon: .highlighter, label: "Mark the selected words", did: "marked text") {
    require first >= 0 and last >= first "A mark covers the words a selection holds"
    mark_text(d, p, style, colour, run_rects_between(p, first, last))
}

/// The boxes, in points of `p`, of the runs from `first` to `last`, both counted in.
fn run_rects_between(p: Page, first: Int, last: Int) -> List<Float> {
    count_between(first, last).flat_map(i -> run_box(p, i))
}

/// Draws ink on a page: one list of points a stroke, its points as x and y in turn in fractions
/// of the page's box.
action draw_ink(d: Document, p: Page, colour: Text, strokes: List<List<Float>>) -> Mark (
    icon: .pencil,
    label: "Draw",
    did: "drew on a page",
) {
    let made = d.marks.insert(Mark(page: p, kind: .ink, colour: colour))
    for stroke in strokes {
        made.strokes.insert(Stroke(points: stroke))
    }
    made
}

/// Puts a sticky note on a page where the person pressed, with nothing written in it yet: what
/// it says is written in the conversation it starts.
action new_note(d: Document, p: Page, x: Float, y: Float, colour: Text) -> Mark (
    icon: .message_square,
    label: "Add note",
    did: "added a note",
) {
    let made = d.marks.insert(Mark(page: p, kind: .note, colour: colour, x: x, y: y))
    d.comments.insert(Comment(mark: .some(made), page: p, words: ""))
    made
}

/// Adds a note with these words at the top of a page: for the palette, a script and an agent.
action add_note(d: Document, p: Page, words: Text) (icon: .message_square, did: "added a note") {
    let made = new_note(d, p, 64.0, 64.0, "#facc15")
    write_mark(d, made, words)
    if let about = d.comments.find(c -> c.mark == .some(made)) { edit_comment(d, about, words) }
}

/// Puts a box of words on a page.
action new_text_box(d: Document, p: Page, x: Float, y: Float, width: Float, height: Float) -> Mark (
    icon: .text_cursor,
    label: "Add a text box",
    did: "added a text box",
) {
    d.marks.insert(Mark(page: p, kind: .text, colour: "#1c212f", rects: [x, y, width, height]))
}

/// Changes what a mark says, and its box when one is named.
action write_mark(d: Document, m: Mark, words: Text, rects: List<Float> = []) (
    label: "Edit a mark",
    did: "edited a mark",
) {
    m.words = words
    if not rects.is_empty() {
        m.rects = rects
    }
}

/// Puts a note where the person moved it.
action move_mark(d: Document, m: Mark, x: Float, y: Float) (
    label: "Move a note",
    did: "moved a note",
) {
    m.x = x
    m.y = y
}

/// Gives a mark another colour.
action recolour(d: Document, m: Mark, colour: Text) (
    label: "Change a mark's colour",
    did: "recoloured a mark",
) {
    m.colour = colour
}

/// Takes a mark away, and what was said about it with it.
destructive action delete_mark(d: Document, m: Mark) (icon: .trash, label: "Delete mark") {
    for c in d.comments.filter(c -> c.mark == .some(m)) {
        for r in c.replies {
            c.replies.remove(r)
        }
        d.comments.remove(c)
    }
    d.marks.remove(m)
}

/// Says something about a page, or about one of its marks.
action comment_on(d: Document, p: Page, m: Option<Mark>, words: Text) -> Comment (
    icon: .circle_plus,
    label: "Comment",
    did: "commented",
) {
    d.comments.insert(Comment(mark: m, page: p, words: words))
}

/// Answers a comment.
action reply(d: Document, c: Comment, words: Text) -> Reply (
    icon: .reply,
    label: "Reply",
    did: "replied",
) {
    c.replies.insert(Reply(words: words))
}

/// Marks a conversation as resolved, or opens it again.
action resolve_comment(d: Document, c: Comment, resolved: Bool) (
    icon: .check,
    label: "Resolve or reopen a comment",
    did: "changed a comment's state",
) {
    c.resolved = resolved
}

/// Rewrites what a comment says.
action edit_comment(d: Document, c: Comment, words: Text) (
    label: "Edit comment",
    did: "edited a comment",
) {
    c.words = words
    c.body = RichText(words)
}

/// Takes a conversation away; a conversation about a sticky note takes the note with it when it
/// is the last one about it.
destructive action delete_comment(d: Document, c: Comment) (icon: .trash, label: "Delete comment") {
    if let m = c.mark {
        let alone = d.comments.filter(other -> other.mark == .some(m) and other != c).is_empty()
        if alone and m.kind == .note { d.marks.remove(m) }
    }
    for r in c.replies {
        c.replies.remove(r)
    }
    d.comments.remove(c)
}

/// Deletes one answer of a conversation.
destructive action delete_reply(d: Document, c: Comment, r: Reply) (
    icon: .trash,
    label: "Delete reply",
) {
    c.replies.remove(r)
}

/// The name typed signatures carry from now on.
action sign_as(lib: Library, name: Text) (
    icon: .pen_tool,
    label: "Sign as",
    did: "set the signing name",
) {
    lib.signer = name.trim()
}

/// Keeps the signature a person drew, so the next document is signed with one press: the
/// strokes drawn on the pad, x and y in turn in fractions of the signature's box.
action draw_signature(lib: Library, strokes: List<List<Float>>) (
    icon: .pencil,
    label: "Draw a signature",
    did: "drew their signature",
) {
    for stroke in lib.strokes {
        lib.strokes.remove(stroke)
    }
    for stroke in strokes {
        lib.strokes.insert(Stroke(points: stroke))
    }
}

/// Signs a page where the person pressed: the signature they drew, else their name set in the
/// signature's box.
action place_signature(
    d: Document,
    p: Page,
    x: Float,
    y: Float,
    width: Float = 218.0,
    height: Float = 74.0,
) -> Signature uses time (icon: .pen_tool, label: "Sign", did: "signed a page") {
    require can_sign(root Library) "Type the name to sign as, or draw a signature, first"
    let lib = root Library
    let name = lib.signer.trim()
    let made = d.signatures.insert(
        Signature(
            page: p,
            x: x,
            y: y,
            width: width,
            height: height,
            signer: if name == "" { "Signature" } else { name },
            date: "{today}",
        ),
    )
    for stroke in lib.strokes {
        made.strokes.insert(Stroke(points: stroke.points))
    }
    made
}

/// Signs a page with a typed name, in its lower right corner: for the palette, a script and an
/// agent.
action sign_with_name(d: Document, p: Page, signer: Text) uses time (
    icon: .pen_tool,
    did: "signed a page",
) {
    require signer.trim() != "" "A signature names who signed"
    d.signatures.insert(
        Signature(
            page: p,
            x: p.width - 64.0 - 218.0,
            y: p.height - 64.0 - 74.0,
            width: 218.0,
            height: 74.0,
            signer: signer.trim(),
            date: "{today}",
        ),
    )
}

/// Takes a signature off the page.
destructive action delete_signature(d: Document, s: Signature) (
    icon: .trash,
    label: "Delete signature",
) {
    d.signatures.remove(s)
}

/// Fills a form field with words.
action fill_field(d: Document, name: Text, text: Text) (
    label: "Fill in a field",
    did: "filled in the form",
) {
    d.form_text.put(name, text)
}

/// Chooses an option of a form's choice.
action choose_field(d: Document, name: Text, choice: Text) (
    label: "Choose a field's option",
    did: "filled in the form",
) {
    d.form_choice.put(name, choice)
}

/// Turns a form's box on or off.
action check_field(d: Document, name: Text, on: Bool, export: Text = "") (
    label: "Fill in a checkbox",
    did: "filled in the form",
) {
    d.form_checked.put(name, on)
    if export != "" { d.form_export.put(name, export) }
}

/// Remembers where the reader left off.
action remember_reading(d: Document, p: Page, fraction: Float, zoom: Float) (
    label: "Remember the reading position",
) {
    require fraction >= 0.0 and fraction <= 1.0 "A reading position is a share of the page"
    d.reading_page = .some(p)
    d.reading_fraction = fraction
    d.reading_zoom = zoom
}

/// Takes over the mark-up the PDF's own bytes carry: its highlights, notes with their answers,
/// free text and ink become this document's, once per source. The file's own objects are kept,
/// so a copy saved with them shows each mark once.
action adopt_markup(d: Document) (
    label: "Take over the PDF's own mark-up",
    did: "took over the PDF's mark-up",
) {
    if d.imported_source == d.source and d.imported_source != "" {
        return
    }
    match pdf.markup(source_of(d)) {
        .ok(text) => match json.decode<MarkupRow>(text) {
            .ok(held) => {
                let short = d.source.slice(d.source.len() - 16, d.source.len())
                for row in held.marks {
                    if not (row.hidden
                        or not row.readable
                        or row.reply
                        or row.kind == "popup"
                        or row.kind == "other") {
                        let id = "pdf-{short}-{row.page}-{row.index}"
                        if d.adopted.get(id) == .none {
                            match d.pages.at(row.page) {
                                .some(page) => {
                                    let made = d.marks.insert(mark_of(row, page, id), key: id)
                                    if made.kind == .ink {
                                        for stroke in row.strokes {
                                            made.strokes.insert(Stroke(points: stroke))
                                        }
                                    }
                                    if made.kind == .note and row.contents.trim() != "" {
                                        d.comments.insert(
                                            Comment(
                                                mark: .some(made),
                                                page: page,
                                                words: row.contents,
                                            ),
                                        )
                                    }
                                    d.adopted.put(
                                        id,
                                        carried_of(
                                            row.page,
                                            row.index,
                                            row.name,
                                            row.kind,
                                            print_of(made),
                                        ),
                                    )
                                }
                                .none => {}
                            }
                        }
                    }
                }
                for row in held.marks {
                    if not (row.hidden or not row.readable or not row.reply) {
                        if let page = d.pages.at(row.page) {
                            let short_id = "pdf-{short}-{row.page}-{row.in_reply_to ?? 0}"
                            let about = if d.adopted.get(short_id) != .none {
                                d.comments.find(c -> c.page == page)
                            } else { .none }
                            let thread = about ?? comment_on(d, page, .none, "")
                            thread.replies.insert(Reply(words: row.contents))
                            d.adopted.put(
                                "pdf-{short}-{row.page}-{row.index}",
                                carried_of(row.page, row.index, row.name, row.kind, ""),
                            )
                        }
                    }
                }
                d.imported_source = d.source
            }
            .err(why) => {
                d.trouble = why
            }
        },
        .err(why) => {
            d.trouble = why
        }
    }
}

/// Writes one mark the file carried into the document. The mark takes the id its key names, so
/// two readers who take the same file over make one mark, and taking it over again is the same
/// mark.
/// The mark a file's own mark-up names, as this document keeps it: its page, its kind, where it
/// stands and what it says, before anything about it is recorded.
fn mark_of(row: MarkRow, page: Page, id: Text) -> Mark {
    if row.kind == "ink" {
        Mark(
            page: page,
            kind: .ink,
            colour: row.color ?? "#1c212f",
            author: row.author,
            adopted_id: id,
        )
    } else if row.kind == "note" {
        Mark(
            page: page,
            kind: .note,
            colour: row.color ?? "#facc15",
            x: row.rect.at(0) ?? 0.0,
            y: (row.rect.at(1) ?? 0.0) + (row.rect.at(3) ?? 0.0),
            words: row.contents,
            author: row.author,
            adopted_id: id,
        )
    } else if row.kind == "freetext" {
        Mark(
            page: page,
            kind: .text,
            colour: row.color ?? "#1c212f",
            rects: row.rect,
            words: row.contents,
            size: row.size ?? 14.0,
            author: row.author,
            adopted_id: id,
        )
    } else {
        let lines = if row.quads.is_empty() { [row.rect] } else { row.quads }
        Mark(
            page: page,
            kind: .markup,
            style: row.kind,
            colour: row.color ?? "#facc15",
            rects: lines.flat_map(r -> r),
            author: row.author,
            adopted_id: id,
        )
    }
}

/// What a mark looks like now, in the same shape a save compares with the fingerprint it had.
/// Its lists are written as JSON, so a word or a point that looks like the separator cannot make
/// two different marks read the same.
fn print_of(m: Mark) -> Text {
    "{m.words}|{json.encode(m.rects)}|{json.encode(m.strokes.map(s -> s.points))}"
}

/// What was taken over from one of the file's own objects: the page and place it stood at, the
/// name and kind the file gave it, and what the mark looked like then.
fn carried_of(page: Int, index: Int, name: Text, kind: Text, print: Text) -> Text {
    json.encode(["{page}", "{index}", name, kind, print])
}

/// Whether a signature can be placed: a name typed to sign as, or one drawn to reuse.
fn can_sign(lib: Library) -> Bool {
    lib.signer.trim() != "" or lib.strokes.len() > 0
}

/// The PDF's own bytes, as the file the engine reads.
fn source_of(d: Document) -> File {
    File.at(if d.source_name == "" { "Document.pdf" } else { d.source_name }, d.source)
}

/// The bytes a page comes from.
fn file_of(d: Document, p: Page) -> File {
    if p.imported == "" { source_of(d) } else { File.at("page.{p.kind}", p.imported) }
}

/// The words a page shows between the runs `from` and `to`, both counted in, a space between
/// them: what a person selects with the pointer and copies.
fn selected_words(p: Page, from: Int, to: Int) -> Text {
    let first = from.min(to).max(0)
    let finish = from.max(to).min(p.run_words.len() - 1)
    if first > finish {
        ""
    } else {
        count_between(first, finish)
            .map(i -> p.run_words.at(i) ?? "")
            .filter(w -> w != "")
            .join(" ")
    }
}

/// The numbers from `from` to `to`, both counted in.
fn count_between(from: Int, to: Int) -> List<Int> {
    if from > to {
        []
    } else {
        let rest = count_between(from + 1, to)
        [[from], rest].flat_map(x -> x)
    }
}

/// Every word a page shows, in reading order: what Select all takes.
fn page_words(p: Page) -> Text {
    if p.run_words.is_empty() {
        p.words
    } else {
        p.run_words.filter(w -> w != "").join(" ")
    }
}

/// How often `query` stands in the runs `p` shows, the case of its letters aside.
fn hits_on(p: Page, query: Text) -> Int {
    let said = query.trim().lower()
    if said == "" {
        0
    } else {
        p.run_words.filter(w -> w.lower().contains(said)).count()
    }
}

/// Finds the words `query` in the document's pages: each page that says them, with how often,
/// as `Page 3: 2`. An agent reads a PDF's answer without the window.
action find_in(d: Document, query: Text) -> List<Text> (icon: .search, label: "Find in a PDF") {
    require query.trim() != "" "Name the words to find"
    pages_hit(d, query).map(p -> "Page {(d.pages.index_of(p) ?? 0) + 1}: {hits_on(p, query)}")
}

/// Copies the words a page shows between two runs, as selecting them and pressing Copy does.
action copy_selected(d: Document, p: Page, from: Int, to: Int) uses clipboard (
    label: "Copy the selection",
    key: "Ctrl+C",
) {
    let said = selected_words(p, from, to)
    if said != "" { clipboard.copy(said) }
}

/// Copies every word a page shows.
action copy_page(d: Document, p: Page) uses clipboard (icon: .copy, label: "Copy the page's text") {
    clipboard.copy(page_words(p))
}

/// The runs of `p` the box from `start` to `end` covers, as fractions of the page's box: the
/// first and last of them, counted in, or none when the box covers no words.
fn runs_in(p: Page, start: List<Float>, end: List<Float>) -> List<Int> {
    let x1 = (start.at(0) ?? 0.0).min(end.at(0) ?? 0.0)
    let y1 = (start.at(1) ?? 0.0).min(end.at(1) ?? 0.0)
    let x2 = (start.at(0) ?? 0.0).max(end.at(0) ?? 0.0)
    let y2 = (start.at(1) ?? 0.0).max(end.at(1) ?? 0.0)
    let covered = count_between(0, p.run_words.len() - 1)
        .filter(i -> overlaps(p, i, x1, y1, x2, y2))
    if covered.is_empty() {
        []
    } else {
        [covered.first() ?? 0, covered.last() ?? 0]
    }
}

/// Whether the run at `i` of `p`'s runs meets the box, in fractions of the page's box.
fn overlaps(p: Page, i: Int, x1: Float, y1: Float, x2: Float, y2: Float) -> Bool {
    let at = i * 4
    let rx = p.runs.at(at) ?? 0.0
    let ry = p.runs.at(at + 1) ?? 0.0
    let rw = p.runs.at(at + 2) ?? 0.0
    let rh = p.runs.at(at + 3) ?? 0.0
    rx < x2 and rx + rw > x1 and ry < y2 and ry + rh > y1
}

/// The words a page's glyphs make, a space between the lines: what the workspace's search reads.
fn words_of(rows: List<GlyphRow>) -> Text {
    let said = ""
    let line = -1
    for row in rows {
        if row.line != line {
            said = "{said} "
            line = row.line
        }
        said = "{said}{row.text}"
    }
    said.trim()
}

/// Saves a copy of the PDF with the reader's mark-up in it: its pages in the reader's order, its
/// marks, what was said about them, the form's values and the file's own mark-up kept where
/// nobody changed it.
action save_copy(d: Document) -> File (
    icon: .download,
    label: "Save annotated PDF",
    did: "saved a copy with its mark-up",
) {
    let files: List<File> = [source_of(d)]
    for p in d.pages {
        if p.imported != "" and not files.any(f -> f.url == p.imported) {
            files = files + [File.at("page.{p.kind}", p.imported)]
        }
    }
    let pages: List<PageOut> = []
    for p in d.pages {
        let source = if p.imported == "" {
            0
        } else {
            files.index_of(imported_of(files, p)) ?? 0
        }
        let first = d.pages
            .filter(q -> q.source_page == p.source_page and q.imported == p.imported)
            .first()
        pages = [
            pages,
            [
                PageOut(
                    source: source,
                    page: p.source_page - 1,
                    rotation: p.rotation,
                    primary: if let q = first { q.id == p.id } else { false },
                ),
            ],
        ]
            .flat_map(x -> x)
    }
    let marks: List<MarkOut> = []
    let replies: List<ReplyOut> = []
    for m in d.marks {
        if let at = d.pages.index_of(m.page) {
            marks = [marks, [mark_out(d, m, at)]].flat_map(x -> x)
            for c in d.comments.filter(c -> c.mark == .some(m)) {
                for r in c.replies {
                    replies = [
                        replies,
                        [
                            ReplyOut(
                                page: at,
                                x: m.x,
                                y: m.y,
                                author: "",
                                body: r.words,
                                name: r.id,
                                answers: m.id,
                            ),
                        ],
                    ]
                        .flat_map(x -> x)
                }
            }
        }
    }
    let request = SaveRequest(
        pages: pages,
        marks: marks,
        replies: replies,
        values: values_of(d),
        adopted: adopted_of(d),
    )
    match pdf.save(files, json.encode(request)) {
        .ok(file) => file,
        .err(why) => {
            d.save_trouble = why
            File.of_text("The annotated PDF could not be made.txt", why)
        }
    }
}

/// The file a page's bytes are, out of those a save hands over.
fn imported_of(files: List<File>, p: Page) -> File {
    files.find(f -> f.url == p.imported) ?? File.at("page.pdf", p.imported)
}

/// One mark as a save names it.
fn mark_out(d: Document, m: Mark, at: Int) -> MarkOut {
    if m.kind == .markup {
        MarkOut(
            page: at,
            kind: m.style,
            color: .some(m.colour),
            author: m.author,
            contents: "",
            name: name_of(d, m),
            created_ms: 0,
            rects: quads_of(m.rects),
        )
    } else if m.kind == .ink {
        MarkOut(
            page: at,
            kind: "ink",
            color: .some(m.colour),
            author: m.author,
            contents: "",
            name: name_of(d, m),
            created_ms: 0,
            strokes: m.strokes.map(s -> s.points),
        )
    } else if m.kind == .note {
        MarkOut(
            page: at,
            kind: "note",
            color: .some(m.colour),
            author: m.author,
            contents: note_words(d, m),
            name: name_of(d, m),
            created_ms: 0,
            rect: .some([m.x, m.y, 0.0, 0.0]),
        )
    } else {
        MarkOut(
            page: at,
            kind: "freetext",
            color: .some(m.colour),
            author: m.author,
            contents: m.words,
            name: name_of(d, m),
            created_ms: 0,
            rect: .some(box_of(m.rects)),
            size: .some(m.size),
        )
    }
}

/// A mark's rectangles as a save names them, four points to a line.
fn quads_of(rects: List<Float>) -> List<List<Float>> {
    quads_from(rects, 0)
}

/// The quads of `rects` from the point `at` on.
fn quads_from(rects: List<Float>, at: Int) -> List<List<Float>> {
    if at + 3 >= rects.len() {
        return []
    }
    [
        [
            [
                rects.at(at) ?? 0.0,
                rects.at(at + 1) ?? 0.0,
                rects.at(at + 2) ?? 0.0,
                rects.at(at + 3) ?? 0.0,
            ],
        ],
        quads_from(rects, at + 4),
    ]
        .flat_map(x -> x)
}

/// The first rectangle of a list, four points to a box.
fn box_of(rects: List<Float>) -> List<Float> {
    [rects.at(0) ?? 0.0, rects.at(1) ?? 0.0, rects.at(2) ?? 0.0, rects.at(3) ?? 0.0]
}

/// What a mark is named in the file: the file's own `/NM` for a mark it carried, else the mark's
/// id, so a re-export names it the same way.
fn name_of(d: Document, m: Mark) -> Text {
    let carried = d.adopted.get(m.adopted_id) ?? ""
    let fields: List<Text> = match json.decode<List<Text>>(carried) {
        .ok(parsed) => parsed,
        .err(_) => [],
    }
    if let named = fields.at(2) {
        if named != "" {
            return named
        }
    }
    m.id
}

/// What was said about a note, as the file writes a sticky note's words.
fn note_words(d: Document, m: Mark) -> Text {
    if let c = d.comments.find(c -> c.mark == .some(m)) { c.words } else { m.words }
}

/// The form's values as a save names them.
fn values_of(d: Document) -> List<ValueOut> {
    let made: List<ValueOut> = []
    for name in d.form_text.keys() {
        made = [made, [ValueOut(name: name, kind: "text", text: d.form_text.get(name) ?? "")]]
            .flat_map(x -> x)
    }
    for name in d.form_choice.keys() {
        made = [made, [ValueOut(name: name, kind: "choice", text: d.form_choice.get(name) ?? "")]]
            .flat_map(x -> x)
    }
    for name in d.form_checked.keys() {
        made = [
            made,
            [
                ValueOut(
                    name: name,
                    kind: "checked",
                    on: d.form_checked.get(name) ?? false,
                    export: d.form_export.get(name),
                ),
            ],
        ]
            .flat_map(x -> x)
    }
    made
}

/// The file's own marks a save keeps, replaces or removes, by the fingerprint each had when it
/// was taken over.
fn adopted_of(d: Document) -> List<AdoptedOut> {
    let made: List<AdoptedOut> = []
    for id in d.adopted.keys() {
        let carried = d.adopted.get(id) ?? ""
        let fields: List<Text> = match json.decode<List<Text>>(carried) {
            .ok(parsed) => parsed,
            .err(_) => [],
        }
        let kept = fields.at(4) ?? ""
        let now = if let m = d.marks.find(x -> x.adopted_id == id) { print_of(m) } else { "" }
        made = [
            made,
            [
                AdoptedOut(
                    page: (fields.at(0) ?? "0").to_int() ?? 0,
                    index: (fields.at(1) ?? "0").to_int() ?? 0,
                    name: fields.at(2) ?? "",
                    kind: fields.at(3) ?? "",
                    unchanged: now != "" and now == kept,
                ),
            ],
        ]
            .flat_map(x -> x)
    }
    made
}

/// A page as the engine answers it: its number and the size it stands at.
type PageRow {
    page: Int
    width: Float
    height: Float
}

/// The value an object holds under `name`, when it is an object.
fn field_of(value: json.Value, name: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(name),
        .string(text) => .none,
        .array(items) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The number an object holds under `name`, inside the object it holds under `within`.
fn number_in(value: json.Value, within: Text, name: Text) -> Float {
    match field_of(value, within) {
        .some(inner) => number_of(field_of(inner, name)),
        .none => 0.0,
    }
}

/// The number `value` holds, and zero when it holds none.
fn number_of(value: Option<json.Value>) -> Float {
    match value {
        .some(held) => match held {
            .number(number) => number,
            .string(text) => 0.0,
            .object(fields) => 0.0,
            .array(items) => 0.0,
            .bool(flag) => 0.0,
            .null => 0.0,
        },
        .none => 0.0,
    }
}

/// The text `value` holds, and empty when it holds none.
fn text_of(value: Option<json.Value>) -> Text {
    match value {
        .some(held) => match held {
            .string(text) => text,
            .number(number) => "",
            .object(fields) => "",
            .array(items) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// A link's destination, as the file names it: a page counted from 1, or an address.
fn link_text(value: Option<json.Value>) -> Text {
    match value {
        .some(held) => match held {
            .string(text) => text,
            .number(number) => number.to_int().to_text(),
            .object(fields) => "",
            .array(items) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// One character of a page's text, in the page's display space.
type GlyphRow {
    text: Text
    line: Int
    x: Float
    y: Float
    w: Float
    h: Float
    placed: Bool
}

/// What a file holds as pages.
type ImportRow {
    kind: Text
    pages: List<PageRow>
}

/// One entry of a document's contents as the engine lists it.
type OutlineRow {
    title: Text
    page: Int
    level: Int
}

/// One mark the file's own bytes carry.
type MarkRow {
    page: Int
    index: Int
    name: Text
    kind: Text
    color: Option<Text>
    contents: Text
    author: Text
    created: Text
    modified: Text
    rect: List<Float>
    quads: List<List<Float>>
    strokes: List<List<Float>>
    size: Option<Float>
    in_reply_to: Option<Int>
    reply: Bool
    hidden: Bool
    readable: Bool
}

/// The mark-up and the form a file carries.
type MarkupRow {
    marks: List<MarkRow>
    fields: List<FieldRow>
}

/// One form field of a file.
type FieldRow {
    name: Text
    kind: Text
    value: Text
    checked: Bool
    export: Text
    options: List<Text>
    page: Option<Int>
    rect: Option<List<Float>>
    read_only: Bool
    required: Bool
}

/// One page of a copy, as a save names it.
type PageOut {
    source: Int
    page: Int
    rotation: Int
    primary: Bool
}

/// One mark of a copy, in the output page's display space.
type MarkOut {
    page: Int
    kind: Text
    color: Option<Text>
    author: Text
    contents: Text
    name: Text
    created_ms: Int
    rects: List<List<Float>>
    strokes: List<List<Float>>
    rect: Option<List<Float>> = .none
    size: Option<Float> = .none
}

/// One answer written with the annotation it answers.
type ReplyOut {
    page: Int
    x: Float
    y: Float
    author: Text
    body: Text
    name: Text
    answers: Text
}

/// One form value of a copy.
type ValueOut {
    name: Text
    kind: Text
    text: Text = ""
    on: Bool = false
    export: Option<Text> = .none
}

/// One mark of the file's own a copy keeps, replaces or removes.
type AdoptedOut {
    page: Int
    index: Int
    name: Text
    kind: Text
    unchanged: Bool
}

/// Everything a save writes.
type SaveRequest {
    pages: List<PageOut>
    marks: List<MarkOut>
    replies: List<ReplyOut>
    values: List<ValueOut>
    adopted: List<AdoptedOut>
}
