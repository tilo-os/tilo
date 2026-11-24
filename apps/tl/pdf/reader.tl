/// The workspace's PDFs: what can be opened, and how one opens.
// ! The reader's views: the workspace's PDFs, the pages of one, and the panels beside them.
view library(lib: Library) {
    std.page(lib, header: { library_head(lib) }) {
        if lib.documents.is_empty() {
            ui.EmptyState(
                "No PDFs yet",
                message: "Open one from this device, or from the workspace's files.",
                icon: .file,
                action: "Open PDF",
            ) {
                on act: file.choose(open_pdf(lib), accept: ["application/pdf"])
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

/// One PDF in a list: what it is called, how many pages it has and why it cannot be read.
view document_row(d: Document) {
    ui.ListRow(
        title: d.source_name,
        secondary: if d.trouble == "" { pages_said(d) } else { d.trouble },
        mark: if d.trouble == "" { .file } else { .triangle_alert },
        tone: if d.trouble == "" { .neutral } else { .warning },
    )
}

/// The library's own controls: opening a PDF, from this device or from the workspace.
view library_head(lib: Library) {
    row(gap: .related, align: .center) {
        ui.Button("Open PDF", icon: .file_plus, role: .primary) {
            on press: file.choose(open_pdf(lib), accept: ["application/pdf"])
        }
    }
}

/// How many pages a document has, in words.
fn pages_said(d: Document) -> Text {
    let count = d.pages.len()
    if count == 1 { "1 page" } else { "{count} pages" }
}

/// The reader: the pages down the window, the rail beside them, and the review panel with what
/// people said, the contents and the signing.
view reader(d: Document) {
    // What the window is doing: which page it stands on, the zoom in percent, the tool and its
    // colour, and what it says.
    state page_at: Int = at_page(d)
    state zoom: Float = if d.reading_zoom <= 0.0 { 100.0 } else { d.reading_zoom }
    state rail: Bool = true
    state review: Bool = false
    state tab: Int = 0
    state tool: Text = "select"
    state colour: Text = "#facc15"
    state note: Text = ""
    // A selection being dragged on a page: where it started, as fractions of the page.
    state from: Option<List<Float>> = .none
    state to: Option<List<Float>> = .none
    state finding: Bool = false
    state query: Text = ""
    std.page(
        d,
        canvas: true,
        fill: true,
        header: {
            reader_head(d, page_at, zoom, rail, review, tool, colour)
        },
    ) {
        if d.trouble != "" {
            ui.EmptyState(
                "This PDF cannot be opened",
                message: d.trouble,
                icon: .triangle_alert,
                error: true,
            )
        } else if d.pages.is_empty() {
            ui.EmptyState("Laying out the pages", icon: .file)
        } else {
            if finding {
                find_bar(d, query, page_at)
            }
            if note != "" {
                ui.Banner(note, kind: .info) {
                    on act: set(note, "")
                }
            }
            row(gap: .none, fill) {
                if rail {
                    rail_view(d, page_at, zoom, note)
                }
                // The pages, each built as it scrolls into view: turning to one, in the rail or
                // with the window's keys, scrolls the list to its row.
                ui.VirtualList(fill, page_column: true, reveal: at_page_row(d, page_at)) {
                    on select(p: Page): set(page_at, d.pages.index_of(p) ?? 0)
                    for p in d.pages {
                        ui.ListItem(item: p) {
                            page_sheet(d, p, zoom, tool, colour, from, to, note)
                        }
                    }
                }
                if review {
                    review_panel(d, tab)
                }
            }
        }
    }
}

/// The page the window opens on: the one the reader left off at.
fn at_page(d: Document) -> Int {
    if let p = d.reading_page { d.pages.index_of(p) ?? 0 } else { 0 }
}

/// The reader's own controls: turning pages, the zoom, the tool and its colour, the rail and the
/// review panel, and saving a copy with the mark-up.
view reader_head(
    d: Document,
    page_at: State<Int>,
    zoom: State<Float>,
    rail: State<Bool>,
    review: State<Bool>,
    tool: State<Text>,
    colour: State<Text>,
) {
    ui.Toolbar {
        ui.ToolbarButton("Previous", icon: .chevron_left) {
            on press: turn_to(d, stepped(d, page_at, -1), zoom)
                .then(set(page_at, stepped(d, page_at, -1)))
        }
        ui.ToolbarButton("Next", icon: .chevron_right) {
            on press: turn_to(d, stepped(d, page_at, 1), zoom)
                .then(set(page_at, stepped(d, page_at, 1)))
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Zoom out", icon: .zoom_out) {
            on press: set(zoom, (zoom * 0.8).clamp(25.0, 1600.0))
        }
        ui.ToolbarButton("Zoom in", icon: .zoom_in) {
            on press: set(zoom, (zoom * 1.25).clamp(25.0, 1600.0))
        }
        ui.ToolbarButton("Fit width", icon: .expand) {
            on press: set(zoom, 100.0)
        }
        ui.ToolbarSeparator()
        ui.MenuButton(
            "Tools",
            icon: if tool == "ink" {
                .pencil
            } else if tool == "note" {
                .message_square
            } else if tool == "text" {
                .text_cursor
            } else if tool == "sign" {
                .square_pen
            } else if tool == "select" { .mouse_pointer_2 } else { .highlighter },
            icon_only: true,
        ) {
            ui.MenuItem("Select", icon: .mouse_pointer_2) {
                on choose: set(tool, "select")
            }
            ui.MenuItem("Highlight", icon: .highlighter) {
                on choose: set(tool, "highlight")
            }
            ui.MenuItem("Underline", icon: .underline) {
                on choose: set(tool, "underline")
            }
            ui.MenuItem("Strike through", icon: .strikethrough) {
                on choose: set(tool, "strikeout")
            }
            ui.MenuItem("Squiggle", icon: .pencil) {
                on choose: set(tool, "squiggly")
            }
            ui.MenuItem("Draw", icon: .pencil) {
                on choose: set(tool, "ink")
            }
            ui.MenuItem("Note", icon: .message_square) {
                on choose: set(tool, "note")
            }
            ui.MenuItem("Text box", icon: .text_cursor) {
                on choose: set(tool, "text")
            }
            ui.MenuItem("Sign", icon: .square_pen) {
                on choose: set(tool, "sign")
            }
        }
        ui.MenuButton("Colour", icon: .palette, icon_only: true) {
            for c in marker_colours() {
                ui.MenuItem(c.name) {
                    on choose: set(colour, c.value)
                }
            }
        }
        ui.ToolbarSpacer()
        ui.ToolbarButton("Rail", icon: .panel_left, toggled: rail) {
            on press: set(rail, not rail)
        }
        ui.ToolbarButton(
            "Review",
            icon: .message_circle,
            toggled: review,
            dot: open_comments(d) > 0,
        ) {
            on press: set(review, not review)
        }
        ui.ToolbarButton("Save a copy", icon: .download) {
            on press: file.save(save_copy(d), name: "Document (annotated).pdf")
        }
    }
}

/// The colours a marker comes in.
fn marker_colours() -> List<MarkColour> {
    [
        MarkColour(name: "Yellow", value: "#facc15"),
        MarkColour(name: "Green", value: "#4ade80"),
        MarkColour(name: "Pink", value: "#f472b6"),
        MarkColour(name: "Blue", value: "#60a5fa"),
        MarkColour(name: "Purple", value: "#c084fc"),
    ]
}

/// A marker colour, by the name it is chosen by and the colour it draws.
type MarkColour {
    name: Text
    value: Text
}

/// How many conversations about the document are open: what the review button's dot counts.
fn open_comments(d: Document) -> Int {
    d.comments.filter(c -> not c.resolved).count()
}

/// The page `step` places from `at`, kept inside the document.
fn stepped(d: Document, at: Int, step: Int) -> Int {
    (at + step).clamp(0, d.pages.len() - 1)
}

/// The rail of page thumbnails beside the pages: choosing one turns to it, dragging reorders,
/// and its foot takes more pages in.
view rail_view(d: Document, page_at: State<Int>, zoom: State<Float>, note: State<Text>) {
    let at = d.pages.at(page_at)
    ui.PageSidebar(
        aspect: if let p = at { p.width / p.height } else { 0.77 },
        count: d.pages.len(),
        current: page_at + 1,
        header: "Pages",
        add: "Import…",
    ) {
        on select(index: Int): turn_to(d, index, zoom).then(set(page_at, index))
        on reorder(index: Int, at: Int): reorder(d, index, at)
        on add_page: file.choose(import_after(d, at), accept: ["application/pdf", "image/*"])
    }
}

/// The row the page column shows for the page at `at`: what the list scrolls to when the person
/// turns a page.
fn at_page_row(d: Document, at: Int) -> Option<Text> {
    if let p = d.pages.at(at) { .some(p.id) } else { .none }
}

/// Remembers that the reader turned to the page at `at`.
action turn_to(d: Document, at: Int, zoom: Float) (label: "Turn to a page") {
    if let p = d.pages.at(at) { remember_reading(d, p, 0.0, zoom) }
}

/// Puts the page at `index` where the rail dropped it: after the page at `at`.
action reorder(d: Document, index: Int, at: Int) (label: "Move a page", did: "moved a page") {
    let moved = d.pages.at(index)
    let over = d.pages.at(at)
    if let p = moved {
        if let q = over { if p != q { d.pages.move(p, after: q) } }
    }
}

/// Imports pages after `p`: what the rail's add asks the person for.
action import_after(d: Document, p: Option<Page>, file: File) -> Document (label: "Import pages") {
    import_pages(d, p, file)
    d
}

/// One page of the document, drawn as the engine renders it with its marks over it.
view page_sheet(
    d: Document,
    p: Page,
    zoom: State<Float>,
    tool: Text,
    colour: Text,
    from: State<Option<List<Float>>>,
    to: State<Option<List<Float>>>,
    note: State<Text>,
) {
    let picture = d.pictures.find(x -> x.page == p)
    let shown = if let held = picture { held.url } else { "" }
    let width = p.width * zoom * 1dp
    stack(gap: .related) {
        layers(fill) {
            if shown == "" {
                ui.Skeleton(.media, aspect: .portrait)
            } else {
                ui.Image(
                    shown,
                    alt: "Page {p.source_page}",
                    aspect: p.width / p.height,
                    fit: .contain,
                    max: width,
                )
            }
            ui.Drawing(
                mark_shapes(d, p, from, to),
                meaning: .picture,
                label: "Page {p.source_page}, its marks",
                max: width,
                aspect: p.width / p.height,
            ) {
                on press_at(at: List<Float>): press(d, p, at, tool, colour)
                    .then(set(note, pressed_note(tool)))
                    .then(
                        set(from, if tool == "select" { .some(at) } else { .none }),
                    )
                    .then(
                        set(to, if tool == "select" { .some(at) } else { .none }),
                    )
                on drag_at(at: List<Float>): set(to, .some(at))
                on release_at: release(d, p, tool, colour, from, to)
                    .then(set(note, released_note(tool, from, to)))
                    .then(set(from, .none))
                    .then(set(to, .none))
            }
        }
        row(gap: .related, align: .center) {
            ui.Text("Page {p.source_page}", role: .caption, secondary: true)
            ui.Button("Rotate", icon: .rotate_cw, role: .plain, size: .small) {
                on press: rotate_page(d, p)
            }
            ui.Button("More", icon: .ellipsis, role: .plain, size: .small) {
                on press: set(note, "Page {p.source_page} of {d.pages.len()}")
            }
        }
        for f in d.fields.filter(f -> f.page == p.source_page) {
            field_row(d, f)
        }
    }
}

/// What pressing on a page does, by the tool the window holds: places a note, a text box or a
/// signature where the person pressed. A marking tool starts a selection instead, which the
/// window keeps.
action press(d: Document, p: Page, at: List<Float>, tool: Text, colour: Text) (
    label: "Press a page",
) {
    let x = (at.at(0) ?? 0.0) * p.width
    let y = (at.at(1) ?? 0.0) * p.height
    if tool == "note" {
        new_note(d, p, x, y, colour)
    } else if tool == "text" {
        new_text_box(d, p, x, y, 200.0, 32.0)
    } else if tool == "sign" { place_signature(d, p, x, y) }
}

/// What letting go does: a marking tool marks the box dragged over, and ink keeps the stroke.
action release(
    d: Document,
    p: Page,
    tool: Text,
    colour: Text,
    start: Option<List<Float>>,
    end: Option<List<Float>>,
) (label: "Release a page") {
    if tool == "ink" {
        if let e = end {
            draw_ink(d, p, colour, [[e.at(0) ?? 0.0, e.at(1) ?? 0.0]])
        }
    } else if tool != "select" and tool != "note" and tool != "text" and tool != "sign" {
        if let s = start {
            if let e = end {
                mark_text(d, p, tool, colour, box_between(p, s, e))
            }
        }
    }
}

/// What pressing says, by the tool.
fn pressed_note(tool: Text) -> Text {
    if tool == "note" {
        "Added a note"
    } else if tool == "text" {
        "Added a text box"
    } else if tool == "sign" { "Signed the page" } else if tool == "ink" { "Drawing" } else { "" }
}

/// What letting go says, when it marked something.
fn released_note(tool: Text, start: Option<List<Float>>, end: Option<List<Float>>) -> Text {
    if tool == "ink" {
        if end != .none { "Drew on the page" } else { "" }
    } else if tool != "select" and tool != "note" and tool != "text" and tool != "sign" {
        if start != .none and end != .none { "Marked the page" } else { "" }
    } else { "" }
}

/// The box two points of a page's box make, in the page's points.
fn box_between(p: Page, start: List<Float>, end: List<Float>) -> List<Float> {
    let x1 = (start.at(0) ?? 0.0).min(end.at(0) ?? 0.0) * p.width
    let y1 = (start.at(1) ?? 0.0).min(end.at(1) ?? 0.0) * p.height
    let x2 = (start.at(0) ?? 0.0).max(end.at(0) ?? 0.0) * p.width
    let y2 = (start.at(1) ?? 0.0).max(end.at(1) ?? 0.0) * p.height
    [x1, y1, x2 - x1, y2 - y1]
}

/// The shapes drawn over a page: its marks, the signatures on it, and the selection being made.
fn mark_shapes(
    d: Document,
    p: Page,
    from: Option<List<Float>>,
    to: Option<List<Float>>,
) -> List<ui.Shape> {
    let lines = d.marks
        .filter(m -> m.page == p and m.kind == .markup)
        .flat_map(m -> markup_lines(m, p))
    let ink = d.marks.filter(m -> m.page == p and m.kind == .ink).flat_map(m -> ink_strokes(m))
    let boxes = d.marks
        .filter(m -> m.page == p and (m.kind == .text or m.kind == .note))
        .flat_map(m -> box_shapes(m, p))
    let signatures = d.signatures.filter(s -> s.page == p).flat_map(s -> signature_shapes(s, p))
    let picked = if let start = from {
        if let end = to { [selection_shape(p, start, end)] } else { [] }
    } else { [] }
    [lines, ink, boxes, signatures, picked].flat_map(x -> x)
}

/// A text mark's lines over its page.
fn markup_lines(m: Mark, p: Page) -> List<ui.Shape> {
    rect_rows(m.rects, p)
        .map(
            r -> ui.Shape.area(
                x: r.at(0) ?? 0.0,
                y: r.at(1) ?? 0.0,
                w: r.at(2) ?? 0.0,
                h: r.at(3) ?? 0.0,
                round: 0.02,
                colour: m.colour,
            ),
        )
}

/// A mark's rectangles as fractions of its page's box.
fn rect_rows(rects: List<Float>, p: Page) -> List<List<Float>> {
    rect_rows_from(rects, p, 0)
}

/// The rectangles of `rects` from the point `at` on, as fractions of `p`'s box.
fn rect_rows_from(rects: List<Float>, p: Page, at: Int) -> List<List<Float>> {
    if at + 3 >= rects.len() or p.width <= 0.0 or p.height <= 0.0 {
        return []
    }
    let row = [
        (rects.at(at) ?? 0.0) / p.width,
        (rects.at(at + 1) ?? 0.0) / p.height,
        (rects.at(at + 2) ?? 0.0) / p.width,
        (rects.at(at + 3) ?? 0.0) / p.height,
    ]
    [[row], rect_rows_from(rects, p, at + 4)].flat_map(x -> x)
}

/// An ink mark's strokes.
fn ink_strokes(m: Mark) -> List<ui.Shape> {
    m.strokes.map(s -> ui.Shape.stroke(points: s.points, colour: m.colour, width: 0.004))
}

/// A text box's box and words, and a note's marker.
fn box_shapes(m: Mark, p: Page) -> List<ui.Shape> {
    if m.kind == .note {
        let x = if p.width > 0.0 { m.x / p.width } else { 0.0 }
        let y = if p.height > 0.0 { m.y / p.height } else { 0.0 }
        [
            ui.Shape.area(x: x, y: y, w: 0.03, h: 0.03, round: 0.5, colour: m.colour),
            ui.Shape.label(
                x: x + 0.02,
                y: y + 0.01,
                text: m.words,
                colour: "#1c212f",
                size: 0.02,
                anchor: .start,
            ),
        ]
    } else {
        let box = rect_rows(m.rects, p)
        [
            box.map(
                r -> ui.Shape.area(
                    x: r.at(0) ?? 0.0,
                    y: r.at(1) ?? 0.0,
                    w: r.at(2) ?? 0.0,
                    h: r.at(3) ?? 0.0,
                    round: 0.01,
                    colour: "#ffffff00",
                ),
            ),
            [
                ui.Shape.label(
                    x: (box.at(0)?.at(0) ?? 0.0) + 0.005,
                    y: (box.at(0)?.at(1) ?? 0.0) + 0.01,
                    text: m.words,
                    colour: m.colour,
                    size: 0.02,
                    anchor: .start,
                ),
            ],
        ]
            .flat_map(x -> x)
    }
}

/// A signature's box: the signer's name, or the strokes they drew in it.
fn signature_shapes(s: Signature, p: Page) -> List<ui.Shape> {
    let x = if p.width > 0.0 { s.x / p.width } else { 0.0 }
    let y = if p.height > 0.0 { s.y / p.height } else { 0.0 }
    let w = if p.width > 0.0 { s.width / p.width } else { 0.0 }
    let h = if p.height > 0.0 { s.height / p.height } else { 0.0 }
    let drawn = s.strokes.map(
        stroke -> ui.Shape.stroke(
            points: stroke_in(stroke.points, x, y, w, h),
            colour: "#1c212f",
            width: 0.003,
        ),
    )
    if drawn.is_empty() {
        [
            ui.Shape.label(
                x: x + w / 2.0,
                y: y + h / 2.0,
                text: s.signer,
                colour: "#1c212f",
                size: 0.05,
                anchor: .middle,
            ),
        ]
    } else { drawn }
}

/// A stroke of a signature's own box, placed on its page's box.
fn stroke_in(points: List<Float>, x: Float, y: Float, w: Float, h: Float) -> List<Float> {
    stroke_in_from(points, x, y, w, h, 0)
}

/// The points of `points` from the pair `at` on, placed on the page's box.
fn stroke_in_from(
    points: List<Float>,
    x: Float,
    y: Float,
    w: Float,
    h: Float,
    at: Int,
) -> List<Float> {
    if at + 1 >= points.len() {
        return []
    }
    let px = x + (points.at(at) ?? 0.0) * w
    let py = y + (points.at(at + 1) ?? 0.0) * h
    [[px, py], stroke_in_from(points, x, y, w, h, at + 2)].flat_map(v -> v)
}

/// The box a person is dragging over a page, while they drag.
fn selection_shape(p: Page, start: List<Float>, end: List<Float>) -> ui.Shape {
    let x = (start.at(0) ?? 0.0).min(end.at(0) ?? 0.0)
    let y = (start.at(1) ?? 0.0).min(end.at(1) ?? 0.0)
    let w = (start.at(0) ?? 0.0).max(end.at(0) ?? 0.0) - x
    let h = (start.at(1) ?? 0.0).max(end.at(1) ?? 0.0) - y
    ui.Shape.area(x: x, y: y, w: w, h: h, round: 0.0, colour: "#60a5fa55")
}

/// One form field over its page: its words as a field, its box as a switch, its choice as a menu.
view field_row(d: Document, f: Field) {
    if f.kind == "checkbox" or f.kind == "radio" {
        ui.Button(
            f.name,
            icon: if d.form_checked.get(f.name) ?? false { .check } else { .square },
            role: .plain,
            toggled: d.form_checked.get(f.name) ?? false,
        ) {
            on press: check_field(d, f.name, not (d.form_checked.get(f.name) ?? false), f.export)
        }
    } else if f.kind == "choice" {
        row(gap: .related, align: .center) {
            ui.Text(f.name, role: .caption)
            ui.MenuButton(d.form_choice.get(f.name) ?? "Choose", icon: .chevron_down) {
                for option in f.options {
                    ui.MenuItem(option) {
                        on choose: choose_field(d, f.name, option)
                    }
                }
            }
        }
    } else {
        ui.TextInput(
            text: d.form_text.get(f.name) ?? "",
            placeholder: f.name,
            read_only: f.read_only,
        ) {
            on submit(word: Text): fill_field(d, f.name, word)
        }
    }
}

/// The review panel: what people said, the document's contents and the signing.
view review_panel(d: Document, tab: State<Int>) {
    ui.Docked {
        stack(gap: .none) {
            ui.ViewTabs(selected: tab, label: "Review") {
                ui.Segment("Comments")
                ui.Segment("Contents")
                ui.Segment("Signing")
                on change(i): set(tab, i)
            }
            ui.Scroll {
                review_body(d, tab)
            }
        }
    }
}

/// What the review panel shows, by the tab the window holds.
view review_body(d: Document, tab: State<Int>) {
    if tab == 0 {
        comments_panel(d)
    } else if tab == 1 {
        contents_panel(d, tab)
    } else {
        signing_panel(d)
    }
}

/// What people said about the document: each conversation with its answers and what to do with
/// it, and an entry that says something new.
view comments_panel(d: Document) {
    stack(gap: .section) {
        if d.comments.is_empty() {
            ui.Text("Nothing is said about this PDF yet.", role: .caption, secondary: true)
        } else {
            for c in d.comments {
                comment_card(d, c)
            }
        }
        std.add_entry("Say something about this PDF", speak(d))
    }
}

/// Says something about the page the window shows.
action speak(d: Document, words: Text) -> Option<Comment> (label: "Comment") {
    if let page = d.pages.first() { .some(comment_on(d, page, .none, words)) } else { .none }
}

/// One conversation about the document, or about a mark on it.
view comment_card(d: Document, c: Comment) {
    ui.Card {
        row(gap: .related, align: .center) {
            ui.Text(if c.words == "" { "A note" } else { c.words }, role: .label)
            if c.resolved {
                ui.Badge("Resolved", tone: .success)
            }
            ui.MenuButton("More", icon: .ellipsis, icon_only: true, target: c) {
                ui.MenuItem(if c.resolved { "Reopen" } else { "Resolve" }, icon: .check) {
                    on choose: resolve_comment(d, c, not c.resolved)
                }
                ui.MenuItem("Delete", icon: .trash) {
                    on choose: delete_comment(d, c)
                }
            }
        }
        ui.Text("Page {c.page.source_page}", role: .caption, secondary: true)
        for r in c.replies {
            ui.Text(r.words, role: .body)
        }
        ui.TextInput(placeholder: "Reply") {
            on submit(word: Text): reply(d, c, word)
        }
    }
}

/// The document's contents: each entry at its depth, choosing one showing its page.
view contents_panel(d: Document, tab: State<Int>) {
    if d.outlines.is_empty() {
        ui.Text("This PDF has no contents.", role: .caption, secondary: true)
    } else {
        stack(gap: .tight) {
            for entry in d.outlines {
                ui.ListRow(title: entry.title, note: "Page {entry.page}", dense: true) {
                    on activate: set(tab, 0)
                }
            }
        }
    }
}

/// The signing: the name typed signatures carry, the pad a signature is drawn on, and where it
/// is placed.
view signing_panel(d: Document) {
    let lib = root Library
    state strokes: List<List<Float>> = []
    state drawing: List<Float> = []
    stack(gap: .section) {
        ui.Section("Sign as") {
            ui.TextInput(bind: lib.signer, placeholder: "Your name")
        }
        ui.Section("Draw a signature") {
            ui.Drawing(
                [
                    [
                        ui.Shape.area(
                            x: 0.0,
                            y: 0.0,
                            w: 1.0,
                            h: 1.0,
                            round: 0.04,
                            colour: "#ffffff",
                        ),
                    ],
                    strokes.map(s -> ui.Shape.stroke(points: s, colour: "#1c212f", width: 0.01)),
                    [ui.Shape.stroke(points: drawing, colour: "#1c212f", width: 0.01)],
                ]
                    .flat_map(x -> x),
                meaning: .decoration,
                label: "Sign here",
                height: 120.0,
            ) {
                on press_at(at: List<Float>): set(drawing, at)
                on drag_at(at: List<Float>): set(drawing, [drawing, at].flat_map(x -> x))
                on release_at: set(strokes, [strokes, [drawing]].flat_map(x -> x))
                    .then(set(drawing, []))
            }
            row(gap: .related) {
                ui.Button("Keep this signature", icon: .check) {
                    on press: draw_signature(lib, strokes)
                }
                ui.Button("Clear", icon: .eraser, role: .plain) {
                    on press: set(strokes, [])
                }
            }
        }
        ui.Section("Place it") {
            ui.Text(
                "Choose Sign, then press the page where the signature goes.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// The find bar: what to look for, and how often it stands in the document.
view find_bar(d: Document, query: State<Text>, page_at: State<Int>) {
    row(gap: .related, align: .center) {
        ui.TextInput(text: query, placeholder: "Find in this PDF") {
            on submit(word: Text): set(query, word)
        }
        ui.Text("{hits(d, query)} found", role: .caption, secondary: true)
        ui.Button("Previous", icon: .chevron_left, role: .plain, size: .small) {
            on press: set(page_at, (page_at - 1).clamp(0, d.pages.len() - 1))
        }
        ui.Button("Next", icon: .chevron_right, role: .plain, size: .small) {
            on press: set(page_at, (page_at + 1).clamp(0, d.pages.len() - 1))
        }
    }
}

/// How often the words looked for stand in the document, page by page.
fn hits(d: Document, query: Text) -> Int {
    let said = query.trim().lower()
    if said == "" {
        return 0
    }
    d.pages.filter(p -> p.words.lower().contains(said)).count()
}
