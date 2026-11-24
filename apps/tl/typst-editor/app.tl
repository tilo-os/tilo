app so.tilo.typst-editor "Typst" (
    icon: .code,
    category: .office,
    tagline: "Typesets Typst documents, the page beside the source",
    keywords: ["typst", "typesetting", "typeset", "document", "paper", "pdf", "editor", "markup", "satz", "dokument", "schreiben", "typst-editor"],
    document: library.documents,
)

/// How much a problem the typesetter found matters.
enum Severity {
    error
    warning
}

/// One problem the typesetter reported: what it says, and where in the source it stands, its
/// lines and columns counted from 1 (0 where the compiler states none).
type Problem (title: message) {
    severity: Severity = .error
    message: Text
    line: Int = 0
    column: Int = 0
    end_line: Int = 0
    end_column: Int = 0
    hints: List<Text>

    search {
        hide line
        hide column
        hide end_line
        hide end_column
        hide hints
    }
}

/// One page of the typeset document: how big it is, an identity for what it draws, and the
/// pictures it is shown by — the page at the width the window asks for, and its thumbnail.
type Page (untitled: "Page") {
    /// Its width and height as the document lays it out, in points.
    width: Float
    height: Float
    /// What it draws, as the typesetter names it: a page whose identity stands keeps its picture
    /// through a typeset that leaves it as it was.
    look: Text
    /// The address of its picture at the width the window asked for, `tilo-blob://…`; empty until
    /// it is drawn.
    picture: Text = ""
    /// The width its picture was drawn at, in pixels; 0 until it is drawn.
    picture_width: Int = 0
    /// The address of its thumbnail; empty until it is drawn.
    thumb: Text = ""
    /// The width its thumbnail was drawn at, in pixels; 0 until it is drawn.
    thumb_width: Int = 0

    search {
        hide width
        hide height
        hide look
        hide picture
        hide picture_width
        hide thumb
        hide thumb_width
    }
}

/// One heading of the document, read from its source's syntax: its line, counted from 1, how deep
/// it stands, and its words.
type Heading (title: title) {
    title: Text
    line: Int
    level: Int
}

/// A Typst document: its source, the file it is saved as, what the typesetter said of it, and the
/// pages it lays out. The source is the document's own text, so two people typing in it splice
/// each other's words rather than replacing it.
type Document (title: name, untitled: "Untitled.typ") {
    /// What the document is called.
    name: Text
    /// Its source: the Typst text, merged character by character.
    source: Text
    /// The source the last typeset read; one whose source moved past this is typeset again.
    typeset: Text = ""
    /// Whether the last typeset found a document (false while it does not typeset).
    typesets: Bool = false
    /// How long the last typeset took, in milliseconds.
    took_ms: Int = 0
    /// Why it does not typeset, in the typesetter's words; empty while it does.
    trouble: Text = ""
    /// The source that was last written to a file the person chose.
    written: Text = ""
    /// What the typesetter found, blocking problems first.
    problems: Collection<Problem>
    /// What the document lays out, in its order.
    pages: Collection<Page>
    /// Its headings, read from the source's syntax.
    headings: Collection<Heading>
    /// The address of the PDF the last typeset wrote, and what it is called; empty until one did.
    pdf: Text = ""
    pdf_name: Text = ""
    /// The width pages are drawn at, in pixels: the preview's zoom, 0 for the window's own fit.
    shown_width: Int = 0
    /// What the window shows: `split` (the source beside the page), `editor`, `preview`,
    /// `problems` or `outline`.
    showing: Text = "split"
    /// Where the editor goes to, written `line:column:count`, counted from 1; empty for nowhere.
    reveal: Text = ""
    /// How many places were gone to: each ask counts, so the same place shows again.
    reveals: Int merge counter
    /// The page the person reads, so they come back to it.
    reading_page: Option<Page>
    /// Whether the pages shown are the last that typeset, while the source no longer does.
    stale: Bool = false

    search {
        // The source is what search finds the document's lines in. These two are copies of it
        // (what the last typeset read, what was written to a file), so indexing them would list
        // every line twice.
        conceal typeset
        conceal written
        sort name a_z
    }
}

/// The workspace's Typst documents.
type Library {
    documents: Collection<Document>
}

root library: Library

default view library for Library

default view editor for Document

row view document_row for Document

/// A Typst file opens in the editor: Files, the person's own file system and the system's search
/// hand it over as the file it is, and it opens on the document those words are.
primary action open_typ(lib: Library, file: File) -> Document uses files (
    icon: .file_plus,
    label: "Open a Typst document",
    opens: [.typ, "text/plain"],
    did: "opened a Typst document",
) {
    let words = match file.text() {
        .ok(text) => text,
        .err(why) => {
            return lib.documents.insert(Document(name: file.name, source: "", trouble: why))
        }
    }
    let made = lib.documents.insert(Document(name: named(file.name), source: words, written: words))
    typeset(made)
    made
}

/// A new, empty document.
action new_document(lib: Library, name: Text = "Untitled.typ", source: Text = "") -> Document (
    icon: .file_plus,
    label: "New Typst document",
    did: "made a Typst document",
) {
    let made = lib.documents.insert(
        Document(
            name: named(name),
            source: source,
            written: if source == "" { "" } else { source },
        ),
    )
    typeset(made)
    made
}

/// A document leaves the workspace.
destructive action close_document(lib: Library, d: Document) (
    icon: .trash,
    label: "Close the document",
    did: "closed a document",
) {
    lib.documents.remove(d)
}

/// What a document is called.
fn named(name: Text) -> Text {
    let trimmed = name.trim()
    if trimmed == "" {
        "Untitled.typ"
    } else if trimmed.lower().ends_with(".typ") { trimmed } else { "{trimmed}.typ" }
}

/// The library: the workspace's documents, and opening one.
view library(lib: Library) {
    std.page(lib) {
        header: {
            library_head(lib)
        }
        if lib.documents.is_empty() {
            ui.EmptyState(
                "No Typst documents yet",
                message: "Open one from this device, or start a new one.",
                icon: .code,
                action: "New document",
            ) {
                on act: new_document(lib)
            }
        } else {
            ui.VirtualList(selection_follows_focus: true) {
                on activate(d: Document): open(d)
                for d in lib.documents {
                    ui.ListItem(item: d) {
                        document_row(d)
                    }
                }
            }
        }
    }
}

/// One document in a list: what it is called, how many pages it has, and what the typesetter said.
view document_row(d: Document) {
    ui.ListRow(
        title: d.name,
        secondary: if d.trouble != "" {
            d.trouble
        } else if d.pages.is_empty() { "Not typeset yet" } else { pages_said(d) },
        mark: if d.trouble == "" { .code } else { .triangle_alert },
        tone: if d.trouble == "" { .neutral } else { .warning },
    )
}

/// How many pages a document has, in words.
fn pages_said(d: Document) -> Text {
    let pages = d.pages.len()
    let errors = d.problems.filter(p -> p.severity == .error).count()
    let said = if pages == 1 { "1 page" } else { "{pages} pages" }
    if errors == 0 {
        said
    } else if errors == 1 { "{said} · 1 problem" } else { "{said} · {errors} problems" }
}

/// The library's own controls: opening a document, and starting one.
view library_head(lib: Library) {
    ui.Toolbar {
        ui.ToolbarButton("Open", icon: .folder_open) {
            on press: file.choose(open_typ(lib), accept: [".typ", "text/plain"])
        }
        ui.ToolbarButton("New document", icon: .file_plus) {
            on press: new_document(lib)
        }
    }
}

/// The editor: the source, the page it typesets to, and what the typesetter found.
view editor(d: Document) {
    let lib = root Library
    std.page(d, canvas: true, fill: true) {
        header: {
            // Search inside this document: the same search, narrowed to its source, so a line
            // found opens here at it.
            ui.SearchButton(item: d, label: d.name)
            editor_head(d, lib)
        }
        if d.trouble != "" {
            ui.Banner(
                if d.stale {
                    "The page shown is from before this change"
                } else { "This document does not typeset" },
                message: d.trouble,
                kind: .error,
            )
        }
        if d.showing == "editor" {
            source_pane(d)
        } else if d.showing == "preview" {
            preview_pane(d)
        } else if d.showing == "problems" {
            problems_panel(d)
        } else if d.showing == "outline" {
            outline_panel(d)
        } else {
            split(side: .trailing, collapse: .stack) {
                primary(min: 26rem) {
                    source_pane(d)
                }
                secondary(min: 22rem) {
                    preview_pane(d)
                }
            }
        }
    }
}

/// The window's own controls: the file, the typeset, the view, the zoom and the panels.
view editor_head(d: Document, lib: Library) {
    ui.Toolbar {
        ui.ToolbarButton("Open", icon: .folder_open) {
            on press: file.choose(open_typ(lib), accept: [".typ", "text/plain"])
        }
        ui.ToolbarButton("Save", icon: .save) {
            on press: file.save(save_file(d), name: d.name)
        }
        ui.ToolbarButton("Export PDF", icon: .download) {
            on press: file.save(export_pdf(d), name: pdf_name(d))
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Typeset", icon: .play) {
            on press: typeset(d)
        }
        ui.ToolbarButton(
            "Problems",
            icon: .triangle_alert,
            toggled: d.showing == "problems",
            dot: d.problems.filter(p -> p.severity == .error).count() > 0,
        ) {
            on press: problems(d)
        }
        ui.ToolbarButton("Outline", icon: .list_tree, toggled: d.showing == "outline") {
            on press: outline(d)
        }
        ui.ToolbarButton(
            "The source beside the page",
            icon: .columns_2,
            toggled: d.showing == "split",
        ) {
            on press: side_by_side(d)
        }
        ui.ToolbarButton("The source", icon: .file_code, toggled: d.showing == "editor") {
            on press: editor_view(d)
        }
        ui.ToolbarButton("The page", icon: .file, toggled: d.showing == "preview") {
            on press: preview_view(d)
        }
        ui.ToolbarSpacer()
        ui.ToolbarButton("Zoom out", icon: .zoom_out, joined: true) {
            on press: zoom_out(d)
        }
        ui.ToolbarButton("Zoom in", icon: .zoom_in, joined: true) {
            on press: zoom_in(d)
        }
    }
}

/// The zoom a width stands for, in words: the share of a page's own width it is drawn at.
fn zoom_of(width: Int) -> Text {
    let percent = (width.to_float() * 100.0 / 794.0).round()
    "{percent}%"
}

/// The width a page is drawn at, in pixels: the window's zoom, else a width a page reads at.
fn width_for(d: Document) -> Int {
    if d.shown_width <= 0 { 900 } else { d.shown_width }
}

/// The source pane: the editor, and under it what the typesetter said.
view source_pane(d: Document) {
    ui.Docked {
        content: {
            ui.CodeEditor(
                bind: d.source,
                language: .typst,
                min_lines: 20,
                placeholder: "Write a Typst document",
                reveal: if d.reveal == "" { .none } else { .some(d.reveal) },
            )
        }
        foot: {
            status_line(d)
        }
    }
}

/// What the editor says of the document: its pages, what the typesetter found and how long it took.
view status_line(d: Document) {
    row(gap: .related, align: .center, wrap: true) {
        ui.EditableLabel(bind: d.name)
        ui.Text(pages_said(d), role: .caption, secondary: true)
        if d.took_ms > 0 {
            ui.Text("typeset in {d.took_ms} ms", role: .caption, secondary: true)
        }
        ui.Text("{d.source.split("\\n").len()} lines", role: .caption, secondary: true)
        spacer
        if d.pdf != "" {
            ui.Text("PDF ready", role: .caption, secondary: true)
        }
    }
}

/// The page column: the typeset pages down the window, each drawn as the typesetter made it, with
/// the words a person selects over it.
view preview_pane(d: Document) {
    state page_at: Int = at_page(d)
    state from: Option<List<Float>> = .none
    state to: Option<List<Float>> = .none
    state rail: Bool = true
    stack(gap: .none, fill) {
        row(gap: .related, align: .center) {
            ui.Text(
                if d.pages.is_empty() { "" } else { "Page {page_at + 1} of {d.pages.len()}" },
                role: .caption,
                secondary: true,
            )
            spacer
            ui.Text(zoom_of(width_for(d)), role: .caption, secondary: true)
            ui.Button("Zoom out", icon: .zoom_out, icon_only: true, role: .plain, size: .small) {
                on press: zoom_out(d)
            }
            ui.Button("Zoom in", icon: .zoom_in, icon_only: true, role: .plain, size: .small) {
                on press: zoom_in(d)
            }
            ui.Button("Fit width", icon: .expand, icon_only: true, role: .plain, size: .small) {
                on press: fit_width(d, (space.width - 32.0).to_int())
            }
            ui.Button(
                "The pages' thumbnails",
                icon: .panel_left,
                icon_only: true,
                role: .plain,
                size: .small,
                toggled: rail,
            ) {
                on press: set(rail, not rail)
            }
        }
        if d.pages.is_empty() {
            ui.EmptyState(
                if d.typeset == "" {
                    "Typesetting the document"
                } else { "This document lays out no pages" },
                icon: .file,
                inline: true,
            )
        } else {
            row(gap: .none, fill) {
                if rail {
                    page_rail(d, page_at)
                }
                ui.VirtualList(fill, page_column: true, reveal: page_reveal(d, page_at)) {
                    on select(p: Page): set(page_at, d.pages.index_of(p) ?? 0).then(turn_to(d, p))
                    for p in d.pages {
                        ui.ListItem(item: p) {
                            page_sheet(d, p, page_at, from, to)
                        }
                    }
                }
            }
            if to != .none {
                selection_bar(d, page_at, from, to)
            }
        }
    }
}

/// The page the window opens on: the one the person last read.
fn at_page(d: Document) -> Int {
    if let p = d.reading_page { d.pages.index_of(p) ?? 0 } else { 0 }
}

/// The row the page column shows for the page at `at`: what the list scrolls to when the person
/// turns a page.
fn page_reveal(d: Document, at: Int) -> Option<Text> {
    if let p = d.pages.at(at) { .some(p.id) } else { .none }
}

/// The rail of page thumbnails beside the pages: choosing one turns to it.
view page_rail(d: Document, page_at: State<Int>) {
    stack(gap: .related, fill) {
        ui.Text("Pages", role: .caption, secondary: true)
        ui.VirtualList(fill, reveal: page_reveal(d, page_at)) {
            on select(p: Page): set(page_at, d.pages.index_of(p) ?? 0).then(turn_to(d, p))
            for p in d.pages {
                ui.ListItem(item: p) {
                    page_thumb(d, p, page_at)
                }
            }
        }
    }
}

/// One page's thumbnail, with the page it is.
view page_thumb(d: Document, p: Page, page_at: State<Int>) {
    let index = (d.pages.index_of(p) ?? 0) + 1
    stack(gap: .tight) {
        if p.thumb == "" {
            ui.Skeleton(.media, aspect: .portrait)
        } else {
            ui.Image(
                p.thumb,
                alt: "Page {index}",
                aspect: p.width / p.height,
                fit: .contain,
                max: 132dp,
            )
        }
        ui.Text("{index}", role: .caption, secondary: page_at + 1 != index, numeric: true)
    }
}

/// One page, drawn as the typesetter made it, with the selection a person drags over it.
view page_sheet(
    d: Document,
    p: Page,
    page_at: State<Int>,
    from: State<Option<List<Float>>>,
    to: State<Option<List<Float>>>,
) {
    let index = (d.pages.index_of(p) ?? 0) + 1
    let width = if p.picture_width > 0 { p.picture_width.to_float() } else { 600.0 } * 1dp
    stack(gap: .related) {
        layers(fill) {
            if p.picture == "" {
                ui.Skeleton(.media, aspect: .portrait)
            } else {
                ui.Scroll(across: true) {
                    ui.Image(
                        p.picture,
                        alt: "Page {index} of {d.pages.len()}",
                        aspect: p.width / p.height,
                        fit: .contain,
                        max: width,
                    )
                }
            }
            ui.Drawing(
                selection_shapes(from, to),
                meaning: .decoration,
                label: "Page {index}, its words",
                max: width,
                aspect: p.width / p.height,
            ) {
                on press_at(at: List<Float>): set(from, .some(at))
                    .then(set(to, .none))
                    .then(set(page_at, d.pages.index_of(p) ?? 0))
                on drag_at(at: List<Float>): set(to, .some(at))
            }
        }
        ui.Text("Page {index} of {d.pages.len()}", role: .caption, secondary: true)
    }
}

/// The box a person drags over a page, while they drag it: a share of the page's own box.
fn selection_shapes(start: Option<List<Float>>, end: Option<List<Float>>) -> List<ui.Shape> {
    if let from = start {
        if let to = end {
            let x = (from.at(0) ?? 0.0).min(to.at(0) ?? 0.0)
            let y = (from.at(1) ?? 0.0).min(to.at(1) ?? 0.0)
            let w = (from.at(0) ?? 0.0).max(to.at(0) ?? 0.0) - x
            let h = (from.at(1) ?? 0.0).max(to.at(1) ?? 0.0) - y
            [ui.Shape.area(x: x, y: y, w: w, h: h, round: 0.0, colour: "#60a5fa55")]
        } else { [] }
    } else { [] }
}

/// What can be done with what a person selected: copy its words, or put the selection away.
view selection_bar(
    d: Document,
    page_at: State<Int>,
    from: State<Option<List<Float>>>,
    to: State<Option<List<Float>>>,
) {
    row(gap: .related, align: .center) {
        ui.Text("Words selected", role: .caption)
        ui.Button("Copy", icon: .copy, role: .secondary, size: .small) {
            on press: copy_selection(d, page_at, from, to)
        }
        ui.Button("Copy the page", icon: .file, role: .plain, size: .small) {
            on press: copy_page(d, page_at)
        }
        ui.Button("Clear", icon: .x, role: .plain, size: .small) {
            on press: set(from, .none).then(set(to, .none))
        }
    }
}

/// The problems the typesetter found, each one a place the editor goes to.
view problems_panel(d: Document) {
    if d.problems.is_empty() {
        ui.EmptyState(
            if d.typeset == "" { "Typesetting the document" } else { "No problems" },
            message: if d.typesets { "The document typesets cleanly." } else { "" },
            icon: .check,
        )
    } else {
        ui.VirtualList {
            for p in d.problems {
                ui.ListItem(item: p) {
                    ui.ListRow(
                        title: p.message,
                        secondary: if p.line > 0 { "Line {p.line}:{p.column}" } else { "" },
                        note: match p.hints.first() {
                            .some(hint) => hint,
                            .none => "",
                        },
                        mark: if p.severity == .error { .triangle_alert } else { .circle_alert },
                        tone: if p.severity == .error { .danger } else { .warning },
                    ) {
                        on activate: go_to_problem(d, p)
                    }
                }
            }
        }
    }
}

/// The document's headings, each one a line the editor goes to.
view outline_panel(d: Document) {
    if d.headings.is_empty() {
        ui.EmptyState("No headings", message: "A heading starts with `=`.", icon: .list_tree)
    } else {
        ui.VirtualList {
            for h in d.headings {
                ui.ListItem(item: h) {
                    ui.ListRow(
                        title: h.title,
                        note: "Line {h.line}",
                        mark: .heading_1,
                        prefix: if h.level > 1 { "  " } else { "" },
                        dense: true,
                    ) {
                        on activate: go_to_heading(d, h)
                    }
                }
            }
        }
    }
}

/// The ask a typeset makes.
// ---------------------------------------------------------------- typesetting
type CompileAsk {
    source: Text
}

/// The ask one page's picture makes.
type PageAsk {
    source: Text
    page: Int
    width: Int
}

/// The ask a page's words make.
type TextAsk {
    source: Text
    page: Int
}

/// The ask the outline makes.
type OutlineAsk {
    source: Text
}

/// What the typesetter answered one page with.
type PageRow {
    width: Float
    height: Float
    look: Text
}

/// What the typesetter answered a problem with.
type ProblemRow {
    kind: Text
    message: Text
    line: Option<Int>
    column: Option<Int>
    end_line: Option<Int>
    end_column: Option<Int>
    hints: List<Text>
}

/// What the typesetter answered a typeset with.
type CompiledRow {
    ok: Bool
    elapsed_ms: Int
    /// The address of the PDF the typeset wrote, and what it is called; none while it wrote none.
    pdf_url: Option<Text>
    pdf_name: Option<Text>
    pdf_size: Option<Int>
    pdf_trouble: Option<Text>
}

/// What the typesetter answered the outline with.
type HeadingRow {
    line: Int
    level: Int
    title: Text
}

/// One placed glyph of a page.
type CellRow {
    text: Text
    x0: Float
    x1: Float
    top: Float
    bottom: Float
    line: Int
}

/// Typesets the document, as the window and the background do.
action typeset(d: Document) (
    icon: .play,
    label: "Typeset the document",
    did: "typeset a document",
) {
    match engine.call("compile", json.encode(CompileAsk(source: d.source))) {
        .ok(text) => match json.decode<CompiledRow>(text) {
            .ok(out) => {
                d.typeset = d.source
                d.typesets = out.ok
                d.took_ms = out.elapsed_ms
                match engine.call("problems", json.encode(CompileAsk(source: d.source))) {
                    .ok(problems) => match json.decode<List<ProblemRow>>(problems) {
                        .ok(rows) => take_problems(d, rows),
                        .err(why) => {
                            d.trouble = why
                        }
                    },
                    .err(why) => {
                        d.trouble = why
                    }
                }
                if let why = out.pdf_trouble {
                    d.trouble = why
                } else if out.ok {
                    d.trouble = ""
                } else if let first = d.problems.first() {
                    d.trouble = first.message
                }
                if out.ok {
                    match engine.call("pages", json.encode(CompileAsk(source: d.source))) {
                        .ok(pages) => match json.decode<List<PageRow>>(pages) {
                            .ok(rows) => take_pages(d, rows),
                            .err(why) => {
                                d.trouble = why
                            }
                        },
                        .err(why) => {
                            d.trouble = why
                        }
                    }
                    d.stale = false
                    if let url = out.pdf_url {
                        d.pdf = url
                        d.pdf_name = out.pdf_name ?? pdf_name_of(d.name)
                    } else {
                        d.pdf = ""
                    }
                    read_outline(d)
                } else {
                    d.stale = d.pages.len() > 0
                    d.pdf = ""
                }
            }
            .err(why) => {
                d.trouble = why
            }
        },
        .err(why) => {
            d.trouble = "The document could not be typeset: {why}"
        }
    }
}

/// Typesets the document when its source moved past the last typeset.
action typeset_if_stale(d: Document) (label: "Typeset the document when it changes") {
    if d.source != d.typeset { typeset(d) }
}

/// Typesets the document as the source changes: the window beside the source stays live, and the
/// workspace's origin typesets it while no window is open.
trigger typeset_on_change(d: Document) every 1s: typeset_if_stale(d)

/// Lays the problems a typeset reported over the ones the document held, blocking ones first.
action take_problems(d: Document, rows: List<ProblemRow>) {
    let held: List<Problem> = []
    for problem in d.problems {
        held = [held, [problem]].flat_map(x -> x)
    }
    for problem in held {
        d.problems.remove(problem)
    }
    for row in rows {
        d.problems.insert(
            Problem(
                severity: if row.kind == "warning" { .warning } else { .error },
                message: row.message,
                line: row.line ?? 0,
                column: row.column ?? 0,
                end_line: row.end_line ?? 0,
                end_column: row.end_column ?? 0,
                hints: row.hints,
            ),
        )
    }
}

/// Lays the typeset pages over the ones the document held: a page that draws what it drew keeps
/// its picture, and one that draws something else is drawn again.
action take_pages(d: Document, rows: List<PageRow>) {
    let at = 0
    for row in rows {
        if let page = d.pages.at(at) {
            if page.look != row.look {
                page.look = row.look
                page.picture = ""
                page.picture_width = 0
                page.thumb = ""
                page.thumb_width = 0
            }
            page.width = row.width
            page.height = row.height
        } else {
            d.pages.insert(Page(width: row.width, height: row.height, look: row.look))
        }
        at = at + 1
    }
    // Pages the document no longer lays out leave.
    let held: List<Page> = []
    for page in d.pages {
        held = [held, [page]].flat_map(x -> x)
    }
    for page in held {
        if let place = d.pages.index_of(page) {
            if place >= rows.len() { d.pages.remove(page) }
        }
    }
}

/// Reads the document's headings from its source's syntax.
action read_outline(d: Document) {
    match engine.call("outline", json.encode(OutlineAsk(source: d.source))) {
        .ok(text) => match json.decode<List<HeadingRow>>(text) {
            .ok(rows) => {
                let held: List<Heading> = []
                for heading in d.headings {
                    held = [held, [heading]].flat_map(x -> x)
                }
                for heading in held {
                    d.headings.remove(heading)
                }
                for row in rows {
                    d.headings.insert(Heading(line: row.line, level: row.level, title: row.title))
                }
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// Draws one page at `width` pixels wide, and its thumbnail.
action draw_page(d: Document, page: Page, width: Int) (label: "Draw a page") {
    let index = (d.pages.index_of(page) ?? 0) + 1
    if page.picture_width != width or page.picture == "" {
        match engine.picture(
            "picture",
            json.encode(PageAsk(source: d.source, page: index, width: width)),
        ) {
            .ok(file) => {
                page.picture = file.url
                page.picture_width = width
            }
            .err(why) => {
                d.trouble = "The page could not be drawn: {why}"
            }
        }
    }
    if page.thumb == "" {
        match engine.picture(
            "picture",
            json.encode(PageAsk(source: d.source, page: index, width: 132)),
        ) {
            .ok(file) => {
                page.thumb = file.url
                page.thumb_width = 132
            }
            .err(_) => {}
        }
    }
}

/// Draws the next page the window's width asks for, one a second: a long document fills in page by
/// page, without anyone waiting for it.
action draw_next(d: Document) (label: "Draw the next page") {
    if let next = d.pages.find(p -> p.picture_width != width_for(d) or p.picture == "") {
        draw_page(d, next, width_for(d))
    }
}

/// Draws the pages as the source changes.
trigger draw_pages(d: Document) every 1s: draw_next(d)

/// Shows the source beside the page.
// ---------------------------------------------------------------- views and zoom
action side_by_side(d: Document) (icon: .columns_2, label: "Show the source beside the page") {
    d.showing = "split"
}

/// Shows the source alone.
action editor_view(d: Document) (icon: .file_code, label: "Show the source") {
    d.showing = "editor"
}

/// Shows the page alone.
action preview_view(d: Document) (icon: .file, label: "Show the page") {
    d.showing = "preview"
}

/// Shows what the typesetter found, or goes back to the page.
action problems(d: Document) (icon: .triangle_alert, label: "Problems") {
    if d.showing == "problems" {
        d.showing = "split"
    } else {
        d.showing = "problems"
    }
}

/// Shows the document's headings, or goes back to the page.
action outline(d: Document) (icon: .list_tree, label: "Outline") {
    if d.showing == "outline" {
        d.showing = "split"
    } else {
        d.showing = "outline"
    }
}

/// Draws the pages a step wider.
action zoom_in(d: Document) (icon: .zoom_in, label: "Zoom in") {
    d.shown_width = (width_for(d) + 120).clamp(240, 2400)
}

/// Draws the pages a step narrower.
action zoom_out(d: Document) (icon: .zoom_out, label: "Zoom out") {
    d.shown_width = (width_for(d) - 120).clamp(240, 2400)
}

/// Draws the pages as wide as the room they are shown in.
action fit_width(d: Document, width: Int) (icon: .expand, label: "Fit the page's width") {
    d.shown_width = width.clamp(240, 2400)
}

/// Turns to a page, so the person comes back to it.
action turn_to(d: Document, page: Page) (label: "Turn to a page") {
    d.reading_page = .some(page)
}

/// Goes to the place a problem names.
action go_to_problem(d: Document, problem: Problem) (
    icon: .locate_fixed,
    label: "Go to the problem",
) {
    if problem.line > 0 { go_to(d, problem.line, problem.column) }
}

/// Goes to the line a heading stands on.
action go_to_heading(d: Document, heading: Heading) (
    icon: .locate_fixed,
    label: "Go to the heading",
) {
    go_to(d, heading.line, 1)
}

/// Puts the caret at `line`:`column` and shows it, counting each ask so the same place shows again.
action go_to(d: Document, line: Int, column: Int) {
    d.reveals += 1
    d.reveal = "{line}:{column}:{d.reveals}"
    if d.showing == "problems" or d.showing == "outline" {
        d.showing = "split"
    }
}

/// The document as the file it is saved as, marked as written.
// ---------------------------------------------------------------- files
action save_file(d: Document) -> File (
    icon: .save,
    label: "Save the document",
    did: "saved a document",
) {
    d.written = d.source
    File.of_text(d.name, d.source)
}

/// The PDF the last typeset wrote, as the file a person saves.
action export_pdf(d: Document) -> File (icon: .download, label: "Export the PDF") {
    if d.pdf != "" {
        File.at(pdf_name(d), d.pdf)
    } else {
        File.of_text(
            "The document has no PDF yet.txt",
            if d.trouble == "" {
                "Typeset the document before exporting it."
            } else { d.trouble },
        )
    }
}

/// What the document's PDF is called.
fn pdf_name(d: Document) -> Text {
    pdf_name_of(d.name)
}

/// The PDF's name for a document called `name`.
fn pdf_name_of(name: Text) -> Text {
    if name.lower().ends_with(".typ") {
        "{name.slice(0, name.len() - 4)}.pdf"
    } else if name == "" { "document.pdf" } else { "{name}.pdf" }
}

/// Copies the words a person selected on a page.
action copy_selection(
    d: Document,
    page_at: Int,
    from: Option<List<Float>>,
    to: Option<List<Float>>,
) uses clipboard (icon: .copy, label: "Copy the selected words") {
    if let start = from {
        if let end = to {
            clipboard.copy(selection_text(d, page_at, start, end))
        }
    }
}

/// Copies all of a page's words.
action copy_page(d: Document, page_at: Int) uses clipboard (
    icon: .file,
    label: "Copy the page's words",
) {
    if let page = d.pages.at(page_at) {
        let index = page_at + 1
        clipboard.copy(page_text(d, page_at))
    }
}

/// All of a page's words, as the typesetter read them.
action page_text(d: Document, page_at: Int) -> Text {
    words_of_cells(page_words(d, page_at))
}

/// The words of a page inside the box from `start` to `end`.
action selection_text(d: Document, page_at: Int, start: List<Float>, end: List<Float>) -> Text {
    if let page = d.pages.at(page_at) {
        words_within(page, page_words(d, page_at), start, end)
    } else { "" }
}

/// The placed glyphs of one page, as the typesetter read them.
action page_words(d: Document, page_at: Int) -> List<CellRow> {
    let index = page_at + 1
    match engine.call("text", json.encode(TextAsk(source: d.source, page: index))) {
        .ok(text) => match json.decode<List<CellRow>>(text) {
            .ok(cells) => cells,
            .err(why) => {
                d.trouble = why
                []
            }
        },
        .err(why) => {
            d.trouble = why
            []
        }
    }
}

/// All of a page's words, a space between its lines.
fn words_of_cells(cells: List<CellRow>) -> Text {
    let said = ""
    let line = -1
    for cell in cells {
        if cell.line != line {
            said = "{said} "
            line = cell.line
        }
        said = "{said}{cell.text}"
    }
    said.trim()
}

/// The words of a page inside the box from `start` to `end`, each a share of the page's box.
fn words_within(page: Page, cells: List<CellRow>, start: List<Float>, end: List<Float>) -> Text {
    let x1 = (start.at(0) ?? 0.0).min(end.at(0) ?? 0.0)
    let y1 = (start.at(1) ?? 0.0).min(end.at(1) ?? 0.0)
    let x2 = (start.at(0) ?? 0.0).max(end.at(0) ?? 0.0)
    let y2 = (start.at(1) ?? 0.0).max(end.at(1) ?? 0.0)
    let inside: List<CellRow> = []
    for cell in cells {
        let middle_x = (cell.x0 + cell.x1) / 2.0 / page.width
        let middle_y = (cell.top + cell.bottom) / 2.0 / page.height
        if middle_x >= x1 and middle_x <= x2 and middle_y >= y1 and middle_y <= y2 {
            inside = [inside, [cell]].flat_map(x -> x)
        }
    }
    words_of_cells(inside)
}
