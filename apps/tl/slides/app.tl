app so.tilo.slides "Slides" (
    icon: .monitor,
    category: .office,
    tagline: "Presentations you build together, live",
    keywords: ["slides", "slide", "presentation", "presentations", "deck", "decks", "powerpoint", "pptx", "odp", "impress", "keynote", "präsentation", "präsentationen", "folien", "vortrag", "diashow"],
)

/// How a slide arranges what it holds: the shape its content boxes start in.
enum Layout {
    title
    title_content
    two_column
    section
    blank
}

/// What a slide stands on: the deck's surface, its accent, or its deep tone.
enum Background {
    surface
    accent
    deep
}

/// Which hue drives the deck's accent. Every other colour comes from the deck's look.
enum Palette {
    indigo
    emerald
    amber
    rose
    sky
}

/// A deck's own light or dark surfaces, so it looks the same on every screen and in a saved file.
enum Look {
    light
    dark
}

/// What an element is: words, a shape, a line or a picture.
enum ElementKind {
    text (icon: .type)
    rect (icon: .square)
    ellipse (icon: .circle)
    line (icon: .minus)
    image (icon: .image)
}

/// How a text box sets its words.
enum Align {
    left
    center
    right
    justify
}

/// A deck: its slides in the order people see them, and the palette and look its surfaces take.
type Deck (untitled: "Untitled deck") {
    /// The deck's name: its window's title and an exported file's name.
    title: Text = ""
    palette: Palette = .indigo
    look: Look = .light
    slides: Collection<Slide>
}

/// One slide: its layout, what it stands on, its speaker notes, and the elements drawn on it, back
/// to front.
type Slide (untitled: "Slide") {
    layout: Layout = .title_content
    background: Background = .surface
    /// What the speaker says over this slide.
    notes: RichText
    elements: Collection<Element>

    search {
        sort layout a_z
    }
}

/// A text box, shape, line or picture on a slide.
type Element (untitled: "Text") {
    kind: ElementKind = .text
    /// Left, in slide units (the slide is 1280 × 720).
    x: Int = 96
    /// Top.
    y: Int = 90
    /// Width.
    w: Int = 400
    /// Height.
    h: Int = 90
    /// A text box's words: what is typed, with its bold, italic and lists.
    body: RichText
    /// A picture's content address (`tilo-blob://…`).
    src: Text = ""
    /// Fill: a palette role (`accent`, `surface`, `deep`, `text`, …), `none`, or `#RRGGBB`.
    fill: Text = "accent"
    /// Outline colour, by the same roles.
    stroke: Text = "none"
    stroke_width: Float = 0.0
    font_size: Float = 32.0
    align: Align = .left
    /// Bold over all its words, for an element that has none of its own.
    bold: Bool = false
    italic: Bool = false
    /// Corner radius.
    radius: Float = 16.0
}

/// A deck as a file holds it, and as the engine writes one.
type DeckFile {
    title: Text
    palette: Text
    look: Text
    slides: List<SlideFile>
}

/// A slide as a file holds it.
type SlideFile {
    id: Text
    layout: Text
    background: Text
    notes: Text
    elements: List<ElementFile>
}

/// An element as a file holds it: its words as Markdown.
type ElementFile {
    id: Text
    kind: Text
    x: Float
    y: Float
    w: Float
    h: Float
    text: Text
    src: Text
    fill: Text
    stroke: Text
    stroke_width: Float
    font_size: Float
    align: Text
    bold: Bool
    italic: Bool
    radius: Float
}

/// What a new element is: where it stands, and any style it takes.
type NewElement {
    kind: ElementKind
    x: Int = 440
    y: Int = 260
    w: Int = 400
    h: Int = 220
    text: Option<RichText> = .none
    src: Option<Text> = .none
    fill: Option<Text> = .none
    text_color: Option<Text> = .none
    stroke: Option<Text> = .none
    stroke_width: Option<Float> = .none
    font_size: Option<Float> = .none
    align: Option<Align> = .none
    bold: Option<Bool> = .none
    italic: Option<Bool> = .none
    radius: Option<Float> = .none
}

/// A slide written from an outline: what an agent or a person adds in one step.
type Outline {
    title: Option<Text> = .none
    body: Option<Text> = .none
    layout: Option<Layout> = .none
    notes: Option<Text> = .none
}

root deck: Deck

default view deck for Deck

/// A deck starts with one title slide holding an empty title and subtitle, so a live launch opens
/// on nothing that pretends to be content.
initial action start(d: Deck) {
    d.slides.insert(authored_slide(Slide(title), .none, .none, .none, .some(.title)))
}

/// Adds a slide after `after`, or at the end, from the template its layout names.
primary action add_slide(
    d: Deck,
    title: Text = "",
    body: Text = "",
    layout: Layout = .title_content,
    after: Option<Slide> = .none,
) -> Slide (icon: .plus, label: "Add slide", key: "N", did: "added a slide") {
    place_slide(d, authored_slide(title, body, .some(layout), .none, .some(layout)), after)
}

/// Adds several slides in one step, in the order given: a deck written from an outline.
action add_slides(d: Deck, slides: List<Outline>, after: Option<Slide> = .none) (
    icon: .list_plus,
    label: "Add slides",
    did: "added slides",
) {
    let mut at = index_after(d, after)
    for outline in slides {
        let made = authored_slide(
            outline.title ?? "",
            outline.body ?? "",
            outline.layout,
            outline.notes,
            outline.layout ?? .title_content,
        )
        d.slides.insert(made, at: at)
        at += 1
    }
    if let first = d.slides.at(index_after(d, after)) { first } else { first_slide(d) }
}

/// Copies a slide and everything on it, right after it.
action duplicate_slide(d: Deck, s: Slide) -> Slide (
    icon: .copy,
    label: "Duplicate slide",
    key: "Ctrl+D",
    did: "duplicated a slide",
) {
    let made = d.slides.insert(Slide(
        layout: s.layout,
        background: s.background,
        notes: s.notes,
    ))
    for e in s.elements {
        let copy = made.elements.insert(Element(
            kind: e.kind,
            x: e.x,
            y: e.y,
            w: e.w,
            h: e.h,
            src: e.src,
            fill: e.fill,
            stroke: e.stroke,
            stroke_width: e.stroke_width,
            font_size: e.font_size,
            align: e.align,
            bold: e.bold,
            italic: e.italic,
            radius: e.radius,
        ))
        copy.body = e.body
    }
    d.slides.move(made, after: s)
    made
}

/// Deletes a slide. A deck keeps at least one.
destructive action delete_slide(d: Deck, s: Slide) (icon: .trash, label: "Delete slide") {
    require d.slides.len() > 1 "A deck keeps at least one slide"
    d.slides.remove(s)
}

/// Moves a slide to `at`, the place among the slides a drag reports.
action move_slide(d: Deck, s: Slide, at: Int) (label: "Move the slide", did: "moved a slide") {
    d.slides.move(s, to: at)
}

/// Writes a slide's title and body: the first two text boxes, as the templates make them.
action set_text(d: Deck, s: Slide, title: Text = "", body: Text = "") (
    icon: .pencil,
    label: "Set the slide's text",
    did: "set a slide's text",
) {
    require title.trim() != "" or body.trim() != "" "Give a title, a body or both"
    let boxes = s.elements.filter(e -> e.kind == .text)
    if title.trim() != "" {
        if let box = boxes.at(0) {
            box.body = RichText.from_markdown(title)
        } else {
            require false "The slide has no text box for a title; add one"
        }
    }
    if body.trim() != "" {
        if let box = boxes.at(1) {
            box.body = RichText.from_markdown(body)
        } else {
            require false "The slide has no second text box for a body; add one"
        }
    }
}

/// Adds an element to a slide.
action add_element(s: Slide, element: NewElement) -> Element (
    icon: .plus,
    label: "Add an element",
    did: "added an element",
) {
    let body = element.text ?? RichText("")
    let made = s.elements.insert(Element(
        kind: element.kind,
        x: element.x,
        y: element.y,
        w: element.w,
        h: element.h,
        body: body,
        src: element.src ?? "",
        fill: style_fill(element),
        stroke: element.stroke ?? if element.kind == .line { "accent" } else { "none" },
        stroke_width: element.stroke_width ?? if element.kind == .line { 6.0 } else { 0.0 },
        font_size: element.font_size ?? 32.0,
        align: element.align ?? .left,
        bold: element.bold ?? false,
        italic: element.italic ?? false,
        radius: element.radius ?? if element.kind == .rect { 16.0 } else if element.kind == .image { 12.0 } else { 0.0 },
    ))
    require element.kind == .image or (element.src ?? "") == "" "Only a picture takes a source"
    require element.kind == .text or (element.text ?? RichText("")).is_empty() "Only text takes words"
    require element.kind != .image or made.src != "" "A picture needs its source"
    made
}

/// Inserts an element given whole (a paste), with its own words.
action insert_element(s: Slide, element: NewElement) -> Element (
    icon: .clipboard,
    label: "Paste",
    key: "Ctrl+V",
    did: "pasted an element",
) {
    add_element(s, element)
}

/// Deletes an element.
destructive action delete_element(s: Slide, e: Element) (icon: .trash, label: "Delete") {
    s.elements.remove(e)
}

/// Brings an element to the front.
action bring_to_front(s: Slide, e: Element) (icon: .bring_to_front, label: "Bring to front") {
    s.elements.move(e, to: s.elements.len())
}

/// Sends an element to the back.
action send_to_back(s: Slide, e: Element) (icon: .send_to_back, label: "Send to back") {
    s.elements.move(e, to: 0)
}

/// Moves an element to a place on the slide, as a drag or the arrow keys place it.
action move_element(s: Slide, e: Element, x: Int, y: Int) (label: "Move", did: "moved an element") {
    e.x = x
    e.y = y
}

/// Sizes an element, as dragging its edge does.
action resize_element(s: Slide, e: Element, at: List<Int>) (
    label: "Resize",
    did: "resized an element",
) {
    e.w = (at.at(0) ?? e.w).max(16)
    e.h = (at.at(1) ?? e.h).max(16)
}

/// Nudges an element by `dx` and `dy` slide units.
action nudge_element(s: Slide, e: Element, dx: Int, dy: Int) (label: "Move") {
    e.x = (e.x + dx).clamp(20 - e.w, 1280 - 20)
    e.y = (e.y + dy).clamp(20 - e.h, 720 - 20)
}

/// Sets what an element is painted with, as the inspector's swatches do.
action set_style(
    e: Element,
    fill: Option<Text> = .none,
    stroke: Option<Text> = .none,
    stroke_width: Option<Float> = .none,
    font_size: Option<Float> = .none,
    align: Option<Align> = .none,
    bold: Option<Bool> = .none,
    italic: Option<Bool> = .none,
    radius: Option<Float> = .none,
) (icon: .paint_bucket, label: "Style", did: "styled an element") {
    if let fill = fill { e.fill = fill }
    if let stroke = stroke { e.stroke = stroke }
    if let width = stroke_width { e.stroke_width = width }
    if let size = font_size { e.font_size = size }
    if let align = align { e.align = align }
    if let bold = bold { e.bold = bold }
    if let italic = italic { e.italic = italic }
    if let radius = radius { e.radius = radius }
}

/// Sets a slide's layout.
action set_layout(s: Slide, layout: Layout) (icon: .layout, label: "Layout") {
    s.layout = layout
}

/// Sets what a slide stands on.
action set_background(s: Slide, background: Background) (icon: .palette, label: "Background") {
    s.background = background
}

/// Sets the deck's accent hue.
action set_palette(d: Deck, palette: Palette) (icon: .palette, label: "Deck theme") {
    d.palette = palette
}

/// Sets the deck's light or dark surfaces.
action set_look(d: Deck, look: Look) (icon: .sun_moon, label: "Deck look") {
    d.look = look
}

/// Names the deck.
action set_title(d: Deck, title: Text) (icon: .pencil, label: "Name the deck") {
    require title.trim() != "" "Name the deck something"
    d.title = title.trim()
}

/// Writes what the speaker says over a slide.
action set_notes(s: Slide, notes: RichText) (icon: .pencil, label: "Speaker notes") {
    s.notes = notes
}

/// Copies the chosen element to the clipboard as the JSON another deck pastes.
action copy_element(e: Element) uses clipboard (icon: .copy, label: "Copy", key: "Ctrl+C") {
    clipboard.copy(json.encode([ElementFile(
        id: e.id(),
        kind: kind_code(e.kind),
        x: e.x.to_float(),
        y: e.y.to_float(),
        w: e.w.to_float(),
        h: e.h.to_float(),
        text: e.body.markdown(links: e, from: ""),
        src: e.src,
        fill: e.fill,
        stroke: e.stroke,
        stroke_width: e.stroke_width,
        font_size: e.font_size,
        align: align_code(e.align),
        bold: e.bold,
        italic: e.italic,
        radius: e.radius,
    )]))
}

/// Pastes an element from the clipboard onto a slide, nudged clear of the one it came from.
action paste_element(s: Slide) uses clipboard (icon: .clipboard, label: "Paste", key: "Ctrl+V") {
    if let text = clipboard.paste() {
        if let held = json.decode<List<ElementFile>>(text) {
            for f in held {
                s.elements.insert(element_of(f, 24))
            }
        }
    }
}

/// A deck as text: its slides in order, each title, body and other elements with its id, and its
/// speaker notes. An agent reads a deck without the window.
action read_deck(d: Deck) -> Text (icon: .hash, label: "Read deck as text") {
    outline_of(d)
}

/// Replaces the deck with the one a presentation file holds. The file is opened by the app that
/// opens its kind.
action open_presentation(d: Deck, file: File) (
    icon: .file_plus,
    label: "Open presentation",
    did: "opened a presentation",
    opens: [
        "application/vnd.oasis.opendocument.presentation",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        .odp,
        .pptx,
    ],
) {
    adopt(d, file)
}

/// Opens a presentation the person picks, in place of this deck.
action choose_presentation(d: Deck) uses files (icon: .folder_open, label: "Open presentation") {
    file.choose(open_presentation(d), accept: [
        "application/vnd.oasis.opendocument.presentation",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ])
}

/// Reads the deck a presentation file holds.
action read_presentation(file: File) -> Result<Text, Text> (icon: .hash, label: "Read a presentation file") {
    file.presentation()
}

/// The deck as an OpenDocument presentation.
action export_odp(d: Deck) -> File (icon: .download, label: "Export as OpenDocument") {
    File.of_presentation(export_name(d, "odp"), deck_text(d), "odp")
}

/// The deck as a PowerPoint presentation.
action export_pptx(d: Deck) -> File (icon: .download, label: "Export as PowerPoint") {
    File.of_presentation(export_name(d, "pptx"), deck_text(d), "pptx")
}

/// Keeps the deck as an OpenDocument presentation in the person's files.
action save_odp(d: Deck) uses files (icon: .download, label: "Save presentation") {
    file.save(export_odp(d))
}

/// Keeps the deck as a PowerPoint presentation in the person's files.
action save_pptx(d: Deck) uses files (icon: .download, label: "Save as PowerPoint") {
    file.save(export_pptx(d))
}

/// Reads a deck file into this deck: its slides replace what the deck holds.
action adopt(d: Deck, file: File) (label: "Open the presentation", did: "opened a presentation") {
    match file.presentation() {
        .ok(text) => {
            if let held = json.decode<DeckFile>(text) {
                d.title = held.title
                d.palette = palette_of(held.palette)
                d.look = look_of(held.look)
                for old in d.slides.map(x -> x) {
                    d.slides.remove(old)
                }
                for f in held.slides {
                    adopt_slide(d, f)
                }
            }
        }
        .err(_) => {}
    }
}

/// Imports a picture the person picks onto a slide.
action insert_picture(s: Slide) uses files, image (
    icon: .image,
    label: "Insert picture",
) {
    file.choose(add_picture(s), accept: ["image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp", "image/svg+xml"])
}

/// Puts a chosen picture on the slide.
action add_picture(s: Slide, file: File) (label: "Insert picture", did: "added a picture") {
    s.elements.insert(Element(
        kind: .image,
        x: 340,
        y: 168,
        w: 600,
        h: 384,
        src: file.url,
        fill: "surface",
        radius: 12.0,
    ))
}

/// The deck a file holds, kept as this deck: its slides replace what it held.
action import_file(d: Deck, file: File) (
    icon: .file_plus,
    label: "Import the deck",
    did: "imported a deck",
    opens: [
        "application/vnd.oasis.opendocument.presentation",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        .odp,
        .pptx,
    ],
) {
    adopt(d, file)
}

// ---------------------------------------------------------------------------------------------
// Templates and small readings.

/// A slide from the template its layout names.
fn authored_slide(
    title: Text,
    body: Text,
    layout: Option<Layout>,
    notes: Option<Text>,
    fallback: Layout,
) -> Slide {
    let chosen = layout ?? fallback
    let slide = Slide(layout: chosen, background: if chosen == .title or chosen == .section { .accent } else { .surface })
    if let at = notes { slide.notes = RichText.from_markdown(at) }
    let boxes = template_boxes(chosen)
    for (i, box) in boxes {
        let e = slide.elements.insert(box)
        if i == 0 and title.trim() != "" {
            e.body = RichText.from_markdown(title)
        } else if i == 1 and body.trim() != "" {
            e.body = RichText.from_markdown(body)
        }
    }
    slide
}

/// The boxes a layout starts with, in reading order (the first is the title, the second the body).
fn template_boxes(layout: Layout) -> List<(Int, Element)> {
    if layout == .title {
        [
            (0, box_at(text_box(), 96, 250, 1088, 130, 88.0).with_bold(true)),
            (1, box_at(Element(kind: .text, fill: "text-dim"), 96, 386, 1088, 70, 34.0)),
        ]
    } else if layout == .section {
        [
            (0, box_at(text_box(), 105, 300, 1000, 110, 64.0).with_bold(true)),
            (1, box_at(Element(kind: .text, fill: "text-dim"), 110, 450, 1000, 60, 30.0)),
            (2, rect_box(110, 222, 92, 8, "white")),
        ]
    } else if layout == .two_column {
        [
            (0, box_at(text_box(), 96, 70, 1088, 76, 56.0).with_bold(true)),
            (1, box_at(Element(kind: .text), 128, 312, 460, 240, 30.0)),
            (2, rect_box(96, 168, 150, 8, "accent")),
            (3, rect_box(96, 220, 520, 400, "surface-dim")),
            (4, rect_box(664, 220, 520, 400, "accent")),
        ]
    } else if layout == .blank {
        [(0, Element(kind: .text, fill: "text"))]
    } else {
        [
            (0, box_at(text_box(), 96, 90, 1088, 76, 54.0).with_bold(true)),
            (1, box_at(Element(kind: .text), 96, 230, 900, 300, 32.0)),
            (2, rect_box(96, 188, 150, 8, "accent")),
        ]
    }
}

/// A text box with the deck's text ink.
fn text_box() -> Element {
    Element(kind: .text, fill: "text")
}

/// A rectangle.
fn rect_box(x: Int, y: Int, w: Int, h: Int, fill: Text) -> (Int, Element) {
    (9, Element(kind: .rect, x: x, y: y, w: w, h: h, fill: fill, radius: 4.0))
}

/// `e` placed at a box, with a size.
fn box_at(e: Element, x: Int, y: Int, w: Int, h: Int, size: Float) -> Element {
    e.x = x
    e.y = y
    e.w = w
    e.h = h
    e.font_size = size
    e
}

/// The element a new insertion starts with.
fn inserted_element(kind: ElementKind) -> Element {
    if kind == .text {
        Element(kind: .text, x: 440, y: 320, w: 400, h: 90, fill: "text", font_size: 40.0, radius: 0.0)
    } else if kind == .line {
        Element(kind: .line, x: 440, y: 360, w: 360, h: 0, fill: "none", stroke: "accent", stroke_width: 6.0, radius: 0.0)
    } else if kind == .image {
        Element(kind: .image, x: 440, y: 220, w: 400, h: 280, fill: "surface", radius: 12.0)
    } else {
        Element(kind: kind, x: 480, y: 260, w: 320, h: 220)
    }
}

/// A slide inserted after `after`, or at the end.
fn place_slide(d: Deck, slide: Slide, after: Option<Slide>) -> Slide {
    let at = index_after(d, after)
    d.slides.insert(slide, at: at)
    slide
}

/// The place after `after` in `d`, or the end.
fn index_after(d: Deck, after: Option<Slide>) -> Int {
    if let after = after {
        (d.slides.index_of(after) ?? (d.slides.len() - 1)) + 1
    } else {
        d.slides.len()
    }
}

/// The deck's first slide.
fn first_slide(d: Deck) -> Slide {
    if let s = d.slides.at(0) { s } else { d.slides.insert(Slide()) }
}

/// The element a pasted record makes, moved `nudge` units from its place.
fn element_of(f: ElementFile, nudge: Int) -> Element {
    Element(
        kind: kind_of(f.kind),
        x: f.x.to_int() + nudge,
        y: f.y.to_int() + nudge,
        w: f.w.to_int(),
        h: f.h.to_int(),
        body: RichText.from_markdown(f.text),
        src: f.src,
        fill: f.fill,
        stroke: f.stroke,
        stroke_width: f.stroke_width,
        font_size: f.font_size,
        align: align_of(f.align),
        bold: f.bold,
        italic: f.italic,
        radius: f.radius,
    )
}

/// The fill a new element takes, from its fill or its text colour.
fn style_fill(element: NewElement) -> Text {
    if element.kind == .line {
        "none"
    } else if let fill = element.fill {
        fill
    } else if let colour = element.text_color {
        colour
    } else if element.kind == .text {
        "text"
    } else if element.kind == .image {
        "surface"
    } else {
        "accent"
    }
}

/// The deck as the engine reads it.
fn deck_text(d: Deck) -> Text {
    json.encode(DeckFile(
        title: d.title,
        palette: palette_code(d.palette),
        look: look_code(d.look),
        slides: d.slides.map(s -> SlideFile(
            id: s.id(),
            layout: layout_code(s.layout),
            background: background_code(s.background),
            notes: s.notes.markdown(links: s, from: ""),
            elements: s.elements.map(e -> ElementFile(
                id: e.id(),
                kind: kind_code(e.kind),
                x: e.x.to_float(),
                y: e.y.to_float(),
                w: e.w.to_float(),
                h: e.h.to_float(),
                text: e.body.markdown(links: e, from: ""),
                src: e.src,
                fill: e.fill,
                stroke: e.stroke,
                stroke_width: e.stroke_width,
                font_size: e.font_size,
                align: align_code(e.align),
                bold: e.bold,
                italic: e.italic,
                radius: e.radius,
            )),
        )),
    ))
}

/// Keeps one slide a deck file brings.
fn adopt_slide(d: Deck, f: SlideFile) {
    let made = d.slides.insert(Slide(
        layout: layout_of(f.layout),
        background: background_of(f.background),
        notes: RichText.from_markdown(f.notes),
    ))
    for e in f.elements {
        made.elements.insert(element_of(e, 0))
    }
}

/// What an exported file is called: the deck's name, or `Untitled`, with its ending.
fn export_name(d: Deck, ending: Text) -> Text {
    let stem = if d.title.trim() == "" { "Untitled" } else { d.title.trim() }
    "{stem}.{ending}"
}

/// The deck as text, one block per slide: its number and layout, its title, body and other
/// elements each with the id a later call names, and its speaker notes.
fn outline_of(d: Deck) -> Text {
    let mut out = "{d.title} ({d.slides.len()} slides)\n"
    for index in count_up(0, d.slides.len() - 1) {
        if let s = d.slides.at(index) {
            out = "{out}\nSlide {index + 1} ({layout_code(s.layout)}) \{#{s.id()}\}\n"
            let mut titled = false
            for e in s.elements {
                if e.kind != .text {
                    out = "{out}[{e.kind.label()}] \{#{e.id()}\}\n"
                } else {
                    let text = e.body.markdown(links: e, from: "").trim()
                    if text != "" {
                        if not titled {
                            titled = true
                            out = "{out}# "
                        }
                        out = "{out}{text.replaced("\n", "↵")} \{#{e.id()}\}\n"
                    }
                }
            }
            let notes = s.notes.markdown(links: s, from: "").trim()
            if notes != "" {
                out = "{out}Notes: {notes.replaced("\n", "↵")}\n"
            }
        }
    }
    out
}

/// The integers from `first` to `last`, both ends included.
fn count_up(first: Int, last: Int) -> List<Int> {
    if last < first { [] } else { (first..=last).map(n -> n) }
}

/// The heading a slide shows in a list: its largest words, or its number.
fn slide_title(s: Slide, index: Int) -> Text {
    let texts = s.elements.filter(e -> e.kind == .text)
    let largest = texts.sort_by(e -> -e.font_size).first()
    if let e = largest {
        let line = e.body.markdown(links: e, from: "").trim().split("\n").first() ?? ""
        let flat = line.replaced("#", "").trim()
        if flat != "" { flat } else { "Slide {index + 1}" }
    } else {
        "Slide {index + 1}"
    }
}

/// What an element is called in a list: its first line, or the kind's word.
fn element_label(e: Element) -> Text {
    if e.kind == .text {
        let line = e.body.markdown(links: e, from: "").trim().split("\n").first() ?? ""
        let flat = line.replaced("#", "").trim()
        if flat != "" { flat } else { e.kind.label() }
    } else {
        e.kind.label()
    }
}

fn layout_code(l: Layout) -> Text {
    match l {
        .title => "title",
        .title_content => "title_content",
        .two_column => "two_column",
        .section => "section",
        .blank => "blank",
    }
}

fn layout_of(code: Text) -> Layout {
    if code == "title" {
        .title
    } else if code == "two_column" {
        .two_column
    } else if code == "section" {
        .section
    } else if code == "blank" {
        .blank
    } else {
        .title_content
    }
}

fn background_code(b: Background) -> Text {
    match b {
        .surface => "surface",
        .accent => "accent",
        .deep => "deep",
    }
}

fn background_of(code: Text) -> Background {
    if code == "accent" {
        .accent
    } else if code == "deep" {
        .deep
    } else {
        .surface
    }
}

fn palette_code(p: Palette) -> Text {
    match p {
        .indigo => "indigo",
        .emerald => "emerald",
        .amber => "amber",
        .rose => "rose",
        .sky => "sky",
    }
}

fn palette_of(code: Text) -> Palette {
    if code == "emerald" {
        .emerald
    } else if code == "amber" {
        .amber
    } else if code == "rose" {
        .rose
    } else if code == "sky" {
        .sky
    } else {
        .indigo
    }
}

fn look_code(l: Look) -> Text {
    match l {
        .light => "light",
        .dark => "dark",
    }
}

fn look_of(code: Text) -> Look {
    if code == "dark" { .dark } else { .light }
}

fn kind_code(k: ElementKind) -> Text {
    match k {
        .text => "text",
        .rect => "rect",
        .ellipse => "ellipse",
        .line => "line",
        .image => "image",
    }
}

fn kind_of(code: Text) -> ElementKind {
    if code == "rect" {
        .rect
    } else if code == "ellipse" {
        .ellipse
    } else if code == "line" {
        .line
    } else if code == "image" {
        .image
    } else {
        .text
    }
}

fn align_code(a: Align) -> Text {
    match a {
        .left => "left",
        .center => "center",
        .right => "right",
        .justify => "justify",
    }
}

fn align_of(code: Text) -> Align {
    if code == "center" {
        .center
    } else if code == "right" {
        .right
    } else if code == "justify" {
        .justify
    } else {
        .left
    }
}

// ---------------------------------------------------------------------------------------------
// The window: the slide rail, the stage and the properties beside it.

/// A deck in the window: its slides in a rail, the slide being edited, the properties of what is
/// chosen, and the presentation the person runs from here.
view deck(d: Deck) {
    state current: Text = ""
    state chosen: Text = ""
    state presenting: Bool = false
    state presenter: Bool = false
    state notes_shown: Bool = false
    state blanked: Text = ""
    state rail: Bool = true
    state properties: Bool = true
    state no_slide: Bool = false
    ui.Fullscreen(showing: presenting, target: "largest") {
        if presenting {
            present_view(d, current, presenter, notes_shown, blanked)
        } else {
            ui.NavShell(
                content_kind: .page,
                title: if d.title.trim() == "" { "Untitled deck" } else { d.title },
                sidebar_title: "Slides",
            ) {
                on drop(files: List<File>): import_file(d, files.at(0) ?? file_none())
                actions: {
                    ui.Button("Add slide", icon: .plus) {
                        on press: set(current, add_slide(d, after: current_slide(d, current)).id())
                    }
                    ui.Button("Insert", icon: .plus, icon_only: true) {
                        on press: set(current, current)
                    }
                    if not d.slides.is_empty() {
                        ui.Button("Present", icon: .play, role: .primary) {
                            on press: set(current, current).then(set(presenting, true)).then(set(presenter, false)).then(set(notes_shown, false)).then(set(blanked, ""))
                        }
                    }
                    ui.Button("Swap view", icon: .layout, icon_only: true) {
                        on press: set(rail, not rail)
                    }
                    ui.Button("Properties", icon: .panel_right, icon_only: true) {
                        on press: set(properties, not properties)
                    }
                }
                sidebar: {
                    slide_rail(d, current, chosen)
                }
                content: {
                    split(side: .trailing, collapse: .stack) {
                        primary(min: 24rem) {
                            stage_of(d, current_slide(d, current), chosen, rail, properties)
                        }
                        secondary(min: 16rem) {
                            if properties {
                                inspector(d, current_slide(d, current), chosen)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The rail: one row per slide, with its number and heading, a press opening it, and its place
/// among the others changed by the buttons beside it.
view slide_rail(d: Deck, current: State<Text>, chosen: State<Text>) {
    ui.VirtualList(fill) {
        on select(picked: Slide): set(current, picked.id()).then(set(chosen, ""))
        for (index, s) in d.slides.map((s, i) -> (i, s)) {
            ui.ListItem(item: s, selected: s.id() == current) {
                row(gap: .tight, align: .center) {
                    ui.Text("{index + 1}", role: .caption, secondary: true, numeric: true)
                    column(gap: .hairline) {
                        ui.Text(slide_title(s, index), role: .body)
                        if not s.notes.is_empty() {
                            ui.Text("Notes", role: .caption, secondary: true)
                        }
                    }
                    ui.Button("Duplicate", icon: .copy, icon_only: true, role: .plain) {
                        on press: duplicate_slide(d, s)
                    }
                    ui.Button("Delete", icon: .trash, icon_only: true, role: .plain) {
                        on press: delete_slide(d, s)
                    }
                }
            }
        }
    }
}

/// The stage: the slide's elements at their places on a canvas the person can pan and zoom. Pressing
/// an element chooses it; dragging one moves it; dragging its edge sizes it.
view stage_of(
    d: Deck,
    s: Slide,
    chosen: State<Text>,
    rail: Bool,
    properties: Bool,
) {
    stack(fill) {
        ui.Map(
            name: "Slide",
            ground: false,
            fill,
            hint: if s.elements.is_empty() { "Add text, a shape or a picture" } else { "" },
        ) {
            on press(at: List<Int>): set(chosen, "")
            on context(target: Text, at: List<Int>): set(chosen, target)
            on resize(target: Text, at: List<Int>): resize_element(s, element_in(s, target), at)
            on drop(target: host.Resource, at: List<Int>): drop_element(s, target, at)
            for e in s.elements {
                ui.MapPin(
                    name: element_label(e),
                    item: e.id(),
                    x: e.x,
                    y: e.y,
                    width: e.w,
                    height: e.h,
                ) {
                    ui.DragSource(e.id(), title: element_label(e)) {
                        on activate: set(chosen, e.id())
                        element_view(e, chosen)
                    }
                }
            }
        }
        row(gap: .tight, align: .center) {
            for kind in [ElementKind.text, .rect, .ellipse, .line, .image] {
                ui.Button(kind.label(), icon: kind.icon_or(.plus)) {
                    on press: if kind == .image { insert_picture(s) } else { set(chosen, add_element(s, NewElement(kind: kind, x: 440, y: 320)).id()) }
                }
            }
        }
    }
}

/// One element as it is drawn on the slide: its words, its picture, or its shape.
view element_view(e: Element, chosen: State<Text>) {
    if e.kind == .text {
        if e.id() == chosen {
            ui.BlockEditor(bind: e.body, size: e.font_size, align: align_code(e.align))
        } else {
            ui.BlockEditor(e.body, size: e.font_size, align: align_code(e.align))
        }
    } else if e.kind == .image {
        if e.src != "" {
            ui.Image(e.src, fit: .cover, round: e.radius)
        } else {
            ui.EmptyState("Picture", message: "Choose a picture", icon: .image)
        }
    } else {
        ui.Drawing([shape_of(e)], meaning: .decoration, fill, height: e.h.to_float())
    }
}

/// The shape an element's values draw.
fn shape_of(e: Element) -> shape {
    e.kind
}

/// The properties of the chosen element, or of the slide when none is chosen.
view inspector(d: Deck, s: Slide, chosen: State<Text>) {
    stack(padding: .related, gap: .related) {
        ui.Text("Properties", role: .subheading)
        if let e = element_in(s, chosen) {
            ui.Text(element_label(e), role: .body)
            ui.Text("Fill", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for role in ["accent", "accent-soft", "surface", "deep", "text", "none", "success", "warning", "danger", "info"] {
                    ui.Button(role, role: .plain) {
                        on press: set_style(e, fill: .some(role))
                    }
                }
            }
            ui.Text("Outline", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for role in ["none", "text", "accent", "success", "warning", "danger", "info"] {
                    ui.Button(role, role: .plain) {
                        on press: set_style(e, stroke: .some(role), stroke_width: .some(if role == "none" { 0.0 } else { 4.0 }))
                    }
                }
            }
            if e.kind == .text {
                ui.Text("Font", role: .caption, secondary: true)
                row(gap: .tight, align: .center) {
                    ui.NumberInput(value: e.font_size, min: 8.0, max: 160.0, step: 2.0, suffix: "pt", label: "Font size") {
                        on change(size): set_style(e, font_size: .some(size))
                    }
                    ui.Button("Bold", icon: .bold, icon_only: true, on: e.bold) {
                        on press: set_style(e, bold: .some(not e.bold))
                    }
                    ui.Button("Italic", icon: .italic, icon_only: true, on: e.italic) {
                        on press: set_style(e, italic: .some(not e.italic))
                    }
                }
                ui.Text("Alignment", role: .caption, secondary: true)
                row(gap: .tight) {
                    for align in [Align.left, .center, .right, .justify] {
                        ui.Button(align.label(), on: e.align == align) {
                            on press: set_style(e, align: .some(align))
                        }
                    }
                }
            }
            ui.Text("Arrange", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                ui.Button("Bring front", icon: .bring_to_front) {
                    on press: bring_to_front(s, e)
                }
                ui.Button("Send back", icon: .send_to_back) {
                    on press: send_to_back(s, e)
                }
                ui.Button("Duplicate", icon: .copy) {
                    on press: set(chosen, add_element(s, copy_of(e)).id())
                }
                ui.Button("Delete", icon: .trash, role: .destructive) {
                    on press: set(chosen, "").then(delete_element(s, e))
                }
            }
        } else {
            ui.Text(slide_title(s, 0), role: .body)
            ui.Text("Layout", role: .caption, secondary: true)
            ui.Dropdown(
                value: layout_code(s.layout),
                options: [Layout.title, .title_content, .two_column, .section, .blank].map(l -> (layout_code(l), l.label())),
                label: "Layout",
            ) {
                on select(code: Text): set_layout(s, layout_of(code))
            }
            ui.Text("Background", role: .caption, secondary: true)
            row(gap: .tight) {
                for background in [Background.surface, .accent, .deep] {
                    ui.Button(background.label(), on: s.background == background) {
                        on press: set_background(s, background)
                    }
                }
            }
            ui.Text("Deck look", role: .caption, secondary: true)
            row(gap: .tight) {
                for look in [Look.light, .dark] {
                    ui.Button(look.label(), on: d.look == look) {
                        on press: set_look(d, look)
                    }
                }
            }
            ui.Text("Deck theme", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for palette in [Palette.indigo, .emerald, .amber, .rose, .sky] {
                    ui.Button(palette.label(), on: d.palette == palette) {
                        on press: set_palette(d, palette)
                    }
                }
            }
        }
        ui.Text("Speaker notes", role: .caption, secondary: true)
        ui.BlockEditor(bind: s.notes, placeholder: "What you say over this slide")
        ui.Button("Export as PowerPoint", icon: .download) {
            on press: export_pptx(d)
        }
        ui.Button("Export as OpenDocument", icon: .download) {
            on press: export_odp(d)
        }
    }
}

/// The presentation: the slide as large as it goes, the person's own controls over it, and the
/// speaker's notes and the next slide when the presenter view is on.
view present_view(
    d: Deck,
    current: State<Text>,
    presenter: State<Bool>,
    notes_shown: State<Bool>,
    blanked: State<Text>,
) {
    stack(fill) {
        if blanked != "" {
            ui.PageSlide(level: 0) {
                if blanked == "black" {
                    ui.Text("", fill)
                } else {
                    ui.Text("", fill)
                }
            }
        } else if presenter {
            presenter_view(d, current, notes_shown)
        } else if let s = current_slide(d, current) {
            ui.Map(name: "Presentation", ground: true, fill) {
                for e in s.elements {
                    ui.MapPin(name: element_label(e), x: e.x, y: e.y, width: e.w, height: e.h) {
                        element_view(e, State<Text>())
                    }
                }
            }
            if notes_shown {
                ui.Card {
                    ui.BlockEditor(s.notes)
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Previous", icon: .chevron_left) {
                on press: set(current, previous_slide(d, current).id())
            }
            ui.Button("Next", icon: .chevron_right) {
                on press: set(current, next_slide(d, current).id())
            }
            ui.Button("First", icon: .chevron_first) {
                on press: set(current, first_slide(d).id())
            }
            ui.Button("Last", icon: .chevron_last) {
                on press: set(current, last_slide(d).id())
            }
            ui.Button("Notes", icon: .notebook, on: notes_shown) {
                on press: set(notes_shown, not notes_shown)
            }
            ui.Button("Presenter", icon: .panel_right, on: presenter) {
                on press: set(presenter, not presenter)
            }
            ui.Button("Black", icon: .moon) {
                on press: set(blanked, if blanked == "black" { "" } else { "black" })
            }
            ui.Button("White", icon: .sun) {
                on press: set(blanked, if blanked == "white" { "" } else { "white" })
            }
            ui.Button("End", icon: .square, role: .primary) {
                on press: set(presenting_state(), false)
            }
        }
    }
}

/// The presenter's view: the slide being shown, the next one, and what the speaker says.
view presenter_view(d: Deck, current: State<Text>, notes_shown: State<Bool>) {
    split(side: .trailing, collapse: .stack) {
        primary(min: 24rem) {
            if let s = current_slide(d, current) {
                stack {
                    ui.Text(slide_title(s, 0), role: .subheading)
                    ui.Map(name: "Current", ground: true, fill) {
                        for e in s.elements {
                            ui.MapPin(name: element_label(e), x: e.x, y: e.y, width: e.w, height: e.h) {
                                element_view(e, State<Text>())
                            }
                        }
                    }
                }
            }
        }
        secondary(min: 16rem) {
            stack(gap: .related) {
                if let next = next_after(d, current) {
                    ui.Text("Next", role: .subheading)
                    ui.Map(name: "Next slide", ground: true, fill) {
                        for e in next.elements {
                            ui.MapPin(name: element_label(e), x: e.x, y: e.y, width: e.w, height: e.h) {
                                element_view(e, State<Text>())
                            }
                        }
                    }
                } else {
                    ui.Text("End of deck", role: .body, secondary: true)
                }
                ui.Text("Speaker notes", role: .subheading)
                if let s = current_slide(d, current) {
                    ui.BlockEditor(s.notes)
                }
            }
        }
    }
}

/// Where the presentation currently stands.
fn current_slide(d: Deck, current: State<Text>) -> Slide {
    if let s = d.slides.find(x -> x.id() == current) {
        s
    } else {
        first_slide(d)
    }
}

/// The next slide, wrapping past the last.
fn next_slide(d: Deck, current: State<Text>) -> Slide {
    let at = d.slides.index_of(current_slide(d, current)) ?? 0
    if let after = d.slides.at(at + 1) { after } else { first_slide(d) }
}

/// The slide before, wrapping past the first.
fn previous_slide(d: Deck, current: State<Text>) -> Slide {
    let at = d.slides.index_of(current_slide(d, current)) ?? 0
    if at > 0 {
        if let before = d.slides.at(at - 1) { before } else { first_slide(d) }
    } else {
        last_slide(d)
    }
}

/// The deck's last slide.
fn last_slide(d: Deck) -> Slide {
    if let s = d.slides.at(d.slides.len() - 1) { s } else { first_slide(d) }
}

/// The slide after the one showing, when there is one.
fn next_after(d: Deck, current: State<Text>) -> Option<Slide> {
    let at = d.slides.index_of(current_slide(d, current)) ?? 0
    d.slides.at(at + 1)
}

/// The element of a slide with this key, if it is there.
fn element_in(s: Slide, key: Text) -> Element {
    if let e = s.elements.find(x -> x.id() == key) { e } else { missing_element(s) }
}

/// A stand-in for an element that is no longer there: the inspector shows nothing of it.
fn missing_element(s: Slide) -> Element {
    if let e = s.elements.at(0) { e } else { s.elements.insert(Element()) }
}

/// The element a drop on the slide moves, to where it was let go.
action drop_element(s: Slide, target: host.Resource, at: List<Int>) (label: "Move it here") {
    if let e = s.elements.find(x -> x.id() == target.id()) {
        move_element(s, e, at.at(0) ?? e.x, at.at(1) ?? e.y)
    }
}

/// A copy of an element, nudged clear of it.
fn copy_of(e: Element) -> NewElement {
    NewElement(
        kind: e.kind,
        x: e.x + 24,
        y: e.y + 24,
        w: e.w,
        h: e.h,
        text: .some(e.body),
        src: .some(e.src),
        fill: .some(e.fill),
        stroke: .some(e.stroke),
        stroke_width: .some(e.stroke_width),
        font_size: .some(e.font_size),
        align: .some(e.align),
        bold: .some(e.bold),
        italic: .some(e.italic),
        radius: .some(e.radius),
    )
}

/// A file value for a drop that carried none.
fn file_none() -> File {
    File.of_text("", "")
}
