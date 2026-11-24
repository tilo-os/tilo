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
    /// When the deck's presentation started, for the speaker's clock; nothing while it is not
    /// being presented.
    presenting_since: Option<Time> = .none
    /// How long the presentation had run when the speaker paused it, in seconds.
    clock_held: Int = 0
    /// The speaker paused the clock.
    clock_paused: Bool = false
    /// Why the last file asked of this deck did not open; empty once it did or the person closed
    /// the message.
    notice: Text = ""

    search {
        sort title a_z
        // When the talk ran and how its theme looks is no way to find a deck.
        hide palette
        hide look
        hide presenting_since
        hide clock_held
        hide clock_paused
    }
}

/// One slide: its layout, what it stands on, its speaker notes, and the elements drawn on it, back
/// to front.
type Slide (untitled: "Slide") {
    layout: Layout = .title_content
    background: Background = .surface
    /// What the speaker says over this slide.
    notes: RichText
    elements: Collection<Element>
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
    /// A text box's ink, or a shape's fill: a palette role (`accent`, `surface`, `deep`, `text-dim`,
    /// …), `none`, or `#RRGGBB`.
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

/// A slide written from an outline: what an agent or a person adds in one step.
type Outline {
    title: Option<Text> = .none
    body: Option<Text> = .none
    layout: Option<Layout> = .none
    notes: Option<Text> = .none
}

/// The colours of a deck, resolved for its palette and look: what its slides are painted in.
type Colours {
    accent: Text
    accent_soft: Text
    accent_deep: Text
    surface: Text
    surface_dim: Text
    deep: Text
    text: Text
    text_dim: Text
    on_accent: Text
    success: Text
    warning: Text
    danger: Text
    info: Text
}

root deck: Deck

default view deck for Deck

default view slide_page for Slide

default view box_page for Element

/// A deck starts with one title slide holding an empty title and subtitle, so a live launch opens
/// on nothing that pretends to be content.
initial action start(d: Deck) {
    let slide = d.slides.insert(Slide(layout: .title, background: .accent))
    let boxes = template_boxes(.title)
    for box in boxes {
        slide.elements.insert(box)
    }
}

/// Adds a slide after `after`, or at the end, from the template its layout names.
primary action add_slide(
    d: Deck,
    title: Text = "",
    body: Text = "",
    layout: Layout = .title_content,
    after: Option<Slide> = .none,
) -> Slide (icon: .plus, label: "Add slide", did: "added a slide") {
    let made = add_authored_slide(d, title, body, .some(layout), .none, layout, after)
    return made
}

/// Adds a slide from the template its layout names, and answers it.
action add_authored_slide(
    d: Deck,
    title: Text,
    body: Text,
    layout: Option<Layout>,
    notes: Option<Text>,
    fallback: Layout,
    after: Option<Slide>,
) -> Slide {
    let chosen = layout ?? fallback
    let gives_way = starter_gives_way(d, after)
    let made = d.slides.insert(
        Slide(
            layout: chosen,
            background: if chosen == .title or chosen == .section { .accent } else { .surface },
        ),
    )
    if let x = after { d.slides.move(made, after: x) }
    if let said = notes {
        made.notes = RichText.from_markdown(said)
    }
    let boxes = template_boxes(chosen)
    let place = 0
    for box in boxes {
        let e = made.elements.insert(box)
        if place == 0 and title.trim() != "" {
            e.body = RichText.from_markdown(title)
        } else if place == 1 and body.trim() != "" {
            e.body = RichText.from_markdown(body)
        }
        place += 1
    }
    // A deck that only held the empty starter keeps the slide someone made instead of it.
    if let old = gives_way { d.slides.remove(old) }
    return made
}

/// Adds several slides in one step, in the order given: a deck written from an outline, as the
/// JSON a person or an agent hands over.
action add_slides(d: Deck, outline: Text, after: Option<Slide> = .none) -> Result<Int, Text> (
    icon: .list_plus,
    label: "Add slides",
    did: "added slides from an outline",
) {
    match json.decode<List<Outline>>(outline) {
        .ok(slides) => {
            let gives_way = starter_gives_way(d, after)
            let previous = after
            let added = 0
            for outline in slides {
                let made = d.slides.insert(
                    Slide(
                        layout: outline.layout ?? .title_content,
                        background: if outline.layout == .some(.title)
                            or outline.layout == .some(.section) {
                            .accent
                        } else { .surface },
                    ),
                )
                if let x = previous { d.slides.move(made, after: x) }
                previous = .some(made)
                if let notes = outline.notes {
                    made.notes = RichText.from_markdown(notes)
                }
                let boxes = template_boxes(outline.layout ?? .title_content)
                let place = 0
                for box in boxes {
                    let e = made.elements.insert(box)
                    if place == 0 {
                        if let title = outline.title {
                            e.body = RichText.from_markdown(title)
                        }
                    } else if place == 1 {
                        if let body = outline.body {
                            e.body = RichText.from_markdown(body)
                        }
                    }
                    place += 1
                }
                added += 1
            }
            // The empty starter gives way to the deck written from the outline.
            if let old = gives_way { d.slides.remove(old) }
            .ok(added)
        }
        .err(why) => .err(why),
    }
}

/// Copies a slide and everything on it, right after it.
action duplicate_slide(d: Deck, s: Slide) -> Slide (
    icon: .copy,
    label: "Duplicate slide",
    did: "duplicated a slide",
) {
    let made = d.slides.insert(Slide(layout: s.layout, background: s.background, notes: s.notes))
    for e in s.elements {
        let copy = made.elements.insert(
            Element(
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
            ),
        )
        copy.body = e.body
    }
    d.slides.move(made, after: s)
    return made
}

/// Copies the slide the window shows, right after it.
action duplicate_current_slide(d: Deck, showing: Text) (
    icon: .copy,
    label: "Duplicate slide",
    did: "duplicated a slide",
) {
    if let s = d.slides.find(x -> x.id() == showing) { duplicate_slide(d, s) }
}

/// Copies an element in place on its slide, a little down and across from it.
action duplicate_element(s: Slide, e: Element) -> Element (
    icon: .layers,
    label: "Duplicate object",
    did: "duplicated an object",
) {
    let copy = s.elements.insert(
        Element(
            kind: e.kind,
            x: e.x + 16,
            y: e.y + 16,
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
        ),
    )
    copy.body = e.body
    return copy
}

/// Deletes a slide. A deck keeps at least one.
destructive action delete_slide(d: Deck, s: Slide) (
    icon: .trash,
    label: "Delete slide",
    did: "deleted a slide",
) {
    require d.slides.len() > 1 "A deck keeps at least one slide"
    d.slides.remove(s)
}

/// Moves a slide to `at`, the place among the slides a drag reports.
action move_slide(d: Deck, s: Slide, at: Int) (label: "Move the slide", did: "moved a slide") {
    d.slides.move(s, to: at)
}

/// Moves the dragged slide where it was dropped: the slide's id and the id of the slide it was
/// dropped on, as the rail's reorder carries them.
action reorder_slides(d: Deck, ids: List<Text>) (label: "Move the slide", did: "moved a slide") {
    if ids.len() < 2 {
        return
    }
    let moved = d.slides.find(s -> s.id() == (ids.at(0) ?? ""))
    let target = d.slides.find(s -> s.id() == (ids.at(1) ?? ""))
    if let s = moved {
        if let before = target { d.slides.move(s, before: before) }
    }
}

/// Writes a slide's title and body: the first two text boxes, as the templates make them.
action set_text(d: Deck, s: Slide, title: Text = "", body: Text = "") (
    icon: .pencil,
    label: "Set the slide's text",
    did: "set the slide's words",
) {
    require title.trim() != "" or body.trim() != "" "Give a title, a body or both"
    let texts = s.elements.filter(e -> e.kind == .text)
    if title.trim() != "" {
        if let box = texts.at(0) {
            box.body = RichText.from_markdown(title)
        } else {
            let made = s.elements.insert(
                box_at(text_box("text"), 96, 90, 1088, 76, 54.0, .left, true),
            )
            made.body = RichText.from_markdown(title)
        }
    }
    if body.trim() != "" {
        if let box = texts.at(1) {
            box.body = RichText.from_markdown(body)
        } else {
            let made = s.elements.insert(
                box_at(text_box("text"), 96, 230, 900, 300, 32.0, .left, false),
            )
            made.body = RichText.from_markdown(body)
        }
    }
}

/// Adds an element to a slide: what it is, where it stands, and any style it takes.
action add_element(
    s: Slide,
    kind: ElementKind,
    x: Int = 440,
    y: Int = 260,
    w: Int = 400,
    h: Int = 220,
    text: Option<RichText> = .none,
    src: Option<Text> = .none,
    fill: Option<Text> = .none,
    text_color: Option<Text> = .none,
    stroke: Option<Text> = .none,
    stroke_width: Option<Float> = .none,
    font_size: Option<Float> = .none,
    align: Option<Align> = .none,
    bold: Option<Bool> = .none,
    italic: Option<Bool> = .none,
    radius: Option<Float> = .none,
) -> Element (icon: .plus, label: "Add an element", did: "added an object") {
    require kind == .image or (src ?? "") == "" "Only a picture takes a source"
    require kind == .text or (text ?? RichText("")).is_empty() "Only text takes words"
    require kind != .image or (src ?? "") != "" "A picture needs its source"
    let made = s.elements.insert(
        Element(
            kind: kind,
            x: x,
            y: y,
            w: w,
            h: h,
            body: text ?? RichText(""),
            src: src ?? "",
            fill: if kind == .line {
                "none"
            } else if let given = fill {
                given
            } else if let colour = text_color {
                colour
            } else if kind == .text {
                "text"
            } else if kind == .image { "surface" } else { "accent" },
            stroke: stroke ?? if kind == .line { "accent" } else { "none" },
            stroke_width: stroke_width ?? if kind == .line { 6.0 } else { 0.0 },
            font_size: font_size ?? 32.0,
            align: align ?? .left,
            bold: bold ?? false,
            italic: italic ?? false,
            radius: radius
                ?? if kind == .rect { 16.0 } else if kind == .image { 12.0 } else { 0.0 },
        ),
    )
    return made
}

/// Inserts an element given whole, as the JSON another deck copied: a paste from anywhere.
action insert_element(s: Slide, element: Text) -> Result<Element, Text> (
    icon: .clipboard,
    label: "Paste",
    did: "pasted an object",
) {
    match json.decode<ElementFile>(element) {
        .ok(held) => .ok(s.elements.insert(element_of(held, 0))),
        .err(why) => .err(why),
    }
}

/// Deletes an element.
destructive action delete_element(s: Slide, e: Element) (
    icon: .trash,
    label: "Delete",
    did: "deleted an object",
) {
    s.elements.remove(e)
}

/// Deletes the element of `s` whose id is `target`, as the stage's delete names it.
destructive action delete_element_at(s: Slide, target: Text) (did: "deleted an object") {
    if let e = s.elements.find(x -> x.id() == target) { s.elements.remove(e) }
}

/// Brings an element to the front.
action bring_to_front(s: Slide, e: Element) (
    icon: .bring_to_front,
    label: "Bring to front",
    did: "brought an object to the front",
) {
    s.elements.move(e, to: s.elements.len())
}

/// Sends an element to the back.
action send_to_back(s: Slide, e: Element) (
    icon: .send_to_back,
    label: "Send to back",
    did: "sent an object to the back",
) {
    s.elements.move(e, to: 0)
}

/// Moves an element to a place on the slide, as a drag or the arrow keys place it.
action move_element(s: Slide, e: Element, x: Int, y: Int) (label: "Move", did: "moved an object") {
    e.x = x
    e.y = y
}

/// Moves the element `target` names to `at`, as a drag on the stage reports it.
action move_element_to(s: Slide, target: Text, at: List<Int>) (
    label: "Move",
    did: "moved an object",
) {
    if let e = s.elements.find(x -> x.id() == target) {
        move_element(s, e, at.at(0) ?? e.x, at.at(1) ?? e.y)
    }
}

/// Nudges the element `target` names by `dx` and `dy` slide units.
action nudge_element_to(s: Slide, target: Text, dx: Int, dy: Int) (
    label: "Move",
    did: "moved an object",
) {
    if let e = s.elements.find(x -> x.id() == target) {
        e.x = (e.x + dx).clamp(20 - e.w, 1280 - 20)
        e.y = (e.y + dy).clamp(20 - e.h, 720 - 20)
    }
}

/// Sizes an element, as dragging its edge does.
action resize_element(s: Slide, e: Element, at: List<Int>) (
    label: "Resize",
    did: "resized an object",
) {
    e.w = (at.at(0) ?? e.w).max(16)
    e.h = (at.at(1) ?? e.h).max(16)
}

/// Sizes the element `target` names, as a drag on its edge reports it.
action resize_element_to(s: Slide, target: Text, at: List<Int>) (
    label: "Resize",
    did: "resized an object",
) {
    if let e = s.elements.find(x -> x.id() == target) { resize_element(s, e, at) }
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
) (icon: .paint_bucket, label: "Style", did: "restyled an object") {
    if let fill = fill {
        e.fill = fill
    }
    if let stroke = stroke {
        e.stroke = stroke
    }
    if let width = stroke_width {
        e.stroke_width = width
    }
    if let size = font_size {
        e.font_size = size
    }
    if let align = align {
        e.align = align
    }
    if let bold = bold {
        e.bold = bold
    }
    if let italic = italic {
        e.italic = italic
    }
    if let radius = radius {
        e.radius = radius
    }
}

/// Sets a slide's layout.
action set_layout(s: Slide, layout: Layout) (
    icon: .layout_dashboard,
    label: "Layout",
    did: "changed the slide's layout",
) {
    s.layout = layout
}

/// Sets what a slide stands on.
action set_background(s: Slide, background: Background) (
    icon: .palette,
    label: "Background",
    did: "changed the slide's background",
) {
    s.background = background
}

/// Sets the deck's accent hue.
action set_palette(d: Deck, palette: Palette) (
    icon: .palette,
    label: "Deck theme",
    did: "changed the deck's theme",
) {
    d.palette = palette
}

/// Sets the deck's light or dark surfaces.
action set_look(d: Deck, look: Look) (
    icon: .sun,
    label: "Deck look",
    did: "changed the deck's look",
) {
    d.look = look
}

/// Names the deck.
action set_title(d: Deck, title: Text) (
    icon: .pencil,
    label: "Name the deck",
    did: "renamed the deck",
) {
    require title.trim() != "" "Name the deck something"
    d.title = title.trim()
}

/// Writes what the speaker says over a slide.
action set_notes(s: Slide, notes: RichText) (
    icon: .pencil,
    label: "Speaker notes",
    did: "wrote the speaker notes",
) {
    s.notes = notes
}

/// Copies the chosen element to the clipboard as the JSON another deck pastes.
action copy_element(e: Element) uses clipboard (icon: .copy, label: "Copy") {
    clipboard.copy(
        json.encode(
            ElementFile(
                id: e.id(),
                kind: kind_code(e.kind),
                x: e.x.to_float(),
                y: e.y.to_float(),
                w: e.w.to_float(),
                h: e.h.to_float(),
                text: e.body.markdown(links: Map<Text>(), from: ""),
                src: e.src,
                fill: e.fill,
                stroke: e.stroke,
                stroke_width: e.stroke_width,
                font_size: e.font_size,
                align: align_code(e.align),
                bold: e.bold,
                italic: e.italic,
                radius: e.radius,
            ),
        ),
    )
}

/// Cuts the chosen element: its JSON to the clipboard as another deck pastes it, and the element
/// off the slide.
action cut_element(s: Slide, e: Element) uses clipboard (
    icon: .scissors,
    label: "Cut",
    did: "cut an object",
) {
    copy_element(e)
    s.elements.remove(e)
}

/// Pastes an element from the clipboard onto a slide, nudged clear of the one it came from.
action paste_element(s: Slide) uses clipboard (
    icon: .clipboard,
    label: "Paste",
    did: "pasted an object",
) {
    if let text = clipboard.paste() {
        match json.decode<ElementFile>(text) {
            .ok(held) => { s.elements.insert(element_of(held, 24)) }
            .err(_) => {}
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

/// Reads the deck a presentation file holds.
action read_presentation(file: File) -> Result<Text, Text> (
    icon: .hash,
    label: "Read a presentation file",
) {
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

/// Reads a deck file into this deck: its slides replace what the deck holds.
action adopt(d: Deck, file: File) (label: "Open the presentation", did: "opened a presentation") {
    match file.presentation() {
        .ok(text) => match json.decode<DeckFile>(text) {
            .ok(held) => {
                d.title = held.title
                d.palette = palette_of(held.palette)
                d.look = look_of(held.look)
                for old in d.slides.map(x -> x) {
                    d.slides.remove(old)
                }
                for f in held.slides {
                    adopt_slide(d, f)
                }
                d.notice = ""
            }
            .err(why) => {
                d.notice = why
            }
        },
        .err(why) => {
            d.notice = why
        }
    }
}

/// Closes the message a deck shows about a file it could not open.
action clear_notice(d: Deck) (icon: .x, label: "Close the message", did: "closed the message") {
    d.notice = ""
}

/// Puts a chosen picture on the slide.
action add_picture(s: Slide, file: File) (label: "Insert picture", did: "inserted a picture") {
    s.elements.insert(
        Element(
            kind: .image,
            x: 340,
            y: 168,
            w: 600,
            h: 384,
            src: file.url,
            fill: "surface",
            radius: 12.0,
        ),
    )
}

/// The deck a file holds, kept as this deck: its slides replace what it held.
action import_file(d: Deck, file: File) (
    icon: .file_plus,
    label: "Import the deck",
    did: "imported a presentation",
) {
    adopt(d, file)
}

/// Opens the first presentation of a set dropped on the window.
action import_files(d: Deck, files: List<File>) (
    label: "Import the deck",
    did: "imported a presentation",
) {
    if let first = files.at(0) { adopt(d, first) }
}

/// Starts the presentation clock: the speaker's elapsed time runs from now.
action begin_presenting(d: Deck) uses time (
    label: "Start the presentation",
    did: "started the presentation",
) {
    d.presenting_since = .some(now)
    d.clock_held = 0
    d.clock_paused = false
}

/// Stops the presentation clock.
action end_presenting(d: Deck) (label: "End the presentation", did: "ended the presentation") {
    d.presenting_since = .none
    d.clock_held = 0
    d.clock_paused = false
}

/// Holds the clock where it stands, until the speaker starts it again.
action pause_clock(d: Deck) uses time (label: "Pause the clock", did: "paused the clock") {
    if let since = d.presenting_since {
        d.clock_held = (now - since).seconds()
        d.clock_paused = true
    }
}

/// Runs the clock on from where it was held.
action resume_clock(d: Deck) uses time (
    label: "Start the clock again",
    did: "started the clock again",
) {
    if d.clock_paused {
        d.presenting_since = .some(now - Duration(d.clock_held))
        d.clock_held = 0
        d.clock_paused = false
    }
}

/// The boxes a layout starts with, in reading order (the first is the title, the second the body).
/// The untouched starter `after` lets give way: the deck opens on one empty title slide, and the
/// first slide anyone adds takes its place. A slide added after another keeps it.
// ---------------------------------------------------------------------------------------------
// Templates and small readings.
fn starter_gives_way(d: Deck, after: Option<Slide>) -> Option<Slide> {
    let starter = untouched_starter(d)
    if let at = after {
        if let s = starter {
            if s.id() == at.id() {
                return starter
            }
        }
        return .none
    }
    starter
}

/// The deck's one starter slide while nobody has written on it or moved what it holds: the empty
/// title slide a new deck opens on.
fn untouched_starter(d: Deck) -> Option<Slide> {
    if d.slides.len() != 1 {
        return .none
    }
    if let s = d.slides.at(0) {
        if s.layout == .title and s.background == .accent {
            if s.notes.markdown(links: Map<Text>(), from: "").trim() == "" {
                let starters = template_boxes(.title)
                if s.elements.len() == starters.len() {
                    let same = true
                    let at = 0
                    for held in s.elements {
                        if element_signature(held) != element_signature(starters.at(at) ?? held) {
                            same = false
                        }
                        at += 1
                    }
                    if same {
                        return .some(s)
                    }
                }
            }
        }
    }
    .none
}

/// What an element is and how it is placed, as one text: a slide's element is a resource and a
/// template box a record, so the two compare by what they say and not by what they are.
fn element_signature(e: Element) -> Text {
    let body = e.body.markdown(links: Map<Text>(), from: "").trim()
    "{e.kind.label()}|{e.x}|{e.y}|{e.w}|{e.h}|{e.fill}|{e.stroke}|{e.stroke_width}|{e.font_size}|{e.align.label()}|{e.bold}|{e.italic}|{e.radius}|{e.src}|{body}"
}

fn template_boxes(layout: Layout) -> List<Element> {
    if layout == .title {
        [
            box_at(text_box("text"), 96, 250, 1088, 130, 88.0, .left, true),
            box_at(text_box("text-dim"), 96, 386, 1088, 70, 34.0, .center, false),
        ]
    } else if layout == .section {
        [
            box_at(text_box("white"), 105, 300, 1000, 110, 64.0, .left, true),
            box_at(text_box("white"), 110, 450, 1000, 60, 30.0, .left, false),
            rect_box(110, 222, 92, 8, "white"),
        ]
    } else if layout == .two_column {
        [
            box_at(text_box("text"), 96, 70, 1088, 76, 56.0, .left, true),
            box_at(text_box("text"), 128, 312, 460, 240, 30.0, .left, false),
            rect_box(96, 168, 150, 8, "accent"),
            rect_box(96, 220, 520, 400, "surface-dim"),
            rect_box(664, 220, 520, 400, "accent-soft"),
        ]
    } else if layout == .blank {
        [text_box("text")]
    } else {
        [
            box_at(text_box("text"), 96, 90, 1088, 76, 54.0, .left, true),
            box_at(text_box("text"), 96, 230, 900, 300, 32.0, .left, false),
            rect_box(96, 188, 150, 8, "accent"),
        ]
    }
}

/// A text box in `ink`.
fn text_box(ink: Text) -> Element {
    Element(kind: .text, fill: ink)
}

/// A rectangle.
fn rect_box(x: Int, y: Int, w: Int, h: Int, fill: Text) -> Element {
    Element(kind: .rect, x: x, y: y, w: w, h: h, fill: fill, radius: 4.0)
}

/// A text box of `ink` at a box, with a size.
fn box_at(
    e: Element,
    x: Int,
    y: Int,
    w: Int,
    h: Int,
    size: Float,
    align: Align,
    bold: Bool,
) -> Element {
    Element(
        kind: e.kind,
        x: x,
        y: y,
        w: w,
        h: h,
        fill: e.fill,
        font_size: size,
        align: align,
        bold: bold,
        radius: 0.0,
    )
}

/// The element a pasted record makes, moved `nudge` slide units from its place.
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

/// The deck as the engine reads it.
fn deck_text(d: Deck) -> Text {
    json.encode(
        DeckFile(
            title: d.title,
            palette: palette_code(d.palette),
            look: look_code(d.look),
            slides: d.slides.map(
                s -> SlideFile(
                    id: s.id(),
                    layout: layout_code(s.layout),
                    background: background_code(s.background),
                    notes: s.notes.markdown(links: Map<Text>(), from: ""),
                    elements: s.elements.map(
                        e -> ElementFile(
                            id: e.id(),
                            kind: kind_code(e.kind),
                            x: e.x.to_float(),
                            y: e.y.to_float(),
                            w: e.w.to_float(),
                            h: e.h.to_float(),
                            text: e.body.markdown(links: Map<Text>(), from: ""),
                            src: e.src,
                            fill: e.fill,
                            stroke: e.stroke,
                            stroke_width: e.stroke_width,
                            font_size: e.font_size,
                            align: align_code(e.align),
                            bold: e.bold,
                            italic: e.italic,
                            radius: e.radius,
                        ),
                    ),
                ),
            ),
        ),
    )
}

/// Keeps one slide a deck file brings.
action adopt_slide(d: Deck, f: SlideFile) (
    label: "Open the presentation",
    did: "opened a presentation",
) {
    let made = d.slides.insert(
        Slide(
            layout: layout_of(f.layout),
            background: background_of(f.background),
            notes: RichText.from_markdown(f.notes),
        ),
    )
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
    let out = "{d.title} ({d.slides.len()} slides)\n"
    for s in d.slides {
        let index = d.slides.index_of(s) ?? 0
        out = "{out}\nSlide {index + 1} ({layout_code(s.layout)}) \{#{s.id()}\}\n"
        let titled = false
        for e in s.elements {
            if e.kind != .text {
                out = "{out}[{e.kind.label()}] \{#{e.id()}\}\n"
            } else {
                let text = e.body.markdown(links: Map<Text>(), from: "").trim()
                if text != "" {
                    if not titled {
                        titled = true
                        out = "{out}# "
                    }
                    out = "{out}{text.replace("\n", "↵")} \{#{e.id()}\}\n"
                }
            }
        }
        let notes = s.notes.markdown(links: Map<Text>(), from: "").trim()
        if notes != "" {
            out = "{out}Notes: {notes.replace("\n", "↵")}\n"
        }
    }
    return out
}

/// The heading a slide shows in a list: its largest words, or its number.
fn slide_title(s: Slide, index: Int) -> Text {
    let texts = s.elements.filter(e -> e.kind == .text)
    let largest = texts.sort_by(e -> -e.font_size).first()
    if let e = largest {
        let line = e.body.markdown(links: Map<Text>(), from: "").trim().split("\n").first() ?? ""
        let flat = line.replace("#", "").trim()
        if flat != "" { flat } else { "Slide {index + 1}" }
    } else { "Slide {index + 1}" }
}

/// What an element is called in a list: its first line, or the kind's word.
fn element_label(e: Element) -> Text {
    if e.kind == .text {
        let line = e.body.markdown(links: Map<Text>(), from: "").trim().split("\n").first() ?? ""
        let flat = line.replace("#", "").trim()
        if flat != "" { flat } else { e.kind.label() }
    } else { e.kind.label() }
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
    } else if code == "blank" { .blank } else { .title_content }
}

fn background_code(b: Background) -> Text {
    match b {
        .surface => "surface",
        .accent => "accent",
        .deep => "deep",
    }
}

fn background_of(code: Text) -> Background {
    if code == "accent" { .accent } else if code == "deep" { .deep } else { .surface }
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
    } else if code == "rose" { .rose } else if code == "sky" { .sky } else { .indigo }
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
    } else if code == "line" { .line } else if code == "image" { .image } else { .text }
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
    } else if code == "right" { .right } else if code == "justify" { .justify } else { .left }
}

/// The colours the deck is drawn in.
// ---------------------------------------------------------------------------------------------
// The deck's colours, resolved for its palette and look. A slide's colours are its content: it
// looks the same on every screen and in a file it is exported to.
fn deck_colours(d: Deck) -> Colours {
    let light = d.look == .light
    let surface = if light { "#FFFFFF" } else { "#1C1917" }
    let surface_content = if light { "#F5F5F4" } else { "#292524" }
    let text = if light { "#1C1917" } else { "#FAFAF9" }
    let text_dim = if light { "#57534E" } else { "#A8A29E" }
    let accent = match d.palette {
        .indigo => "#4F46E5",
        .emerald => "#10B981",
        .amber => "#F59E0B",
        .rose => "#EC4899",
        .sky => "#0EA5E9",
    }
    let deep = "#0F172A"
    Colours(
        accent: accent,
        accent_soft: mix(accent, surface, 0.78),
        accent_deep: mix(accent, deep, 0.62),
        surface: surface,
        surface_dim: mix(surface, surface_content, 0.5),
        deep: deep,
        text: text,
        text_dim: text_dim,
        on_accent: readable_on(accent, "#FFFFFF", text),
        success: "#22C55E",
        warning: "#F59E0B",
        danger: "#EF4444",
        info: "#8B5CF6",
    )
}

/// The colour a slide stands on.
fn slide_ground(d: Deck, s: Slide) -> Text {
    let c = deck_colours(d)
    match s.background {
        .surface => c.surface,
        .accent => c.accent,
        .deep => c.deep,
    }
}

/// The readable ink a text box takes on its slide: the deck's text role follows the ground it
/// stands on, so words never vanish into the slide.
fn slide_ink(d: Deck, s: Slide, role: Text) -> Text {
    let c = deck_colours(d)
    let ground = slide_ground(d, s)
    if role == "text" or role == "accent" {
        readable_on(ground, c.text, c.text_dim)
    } else if role == "text-dim" {
        let soft = mix(readable_on(ground, c.text, c.text_dim), ground, 0.3)
        if contrast(soft, ground) >= 4.5 { soft } else { readable_on(ground, c.text, c.text_dim) }
    } else if role.starts_with("#") { role } else { role_colour(c, role, ground) }
}

/// A shape's fill: the role's colour, empty for nothing.
fn role_fill(d: Deck, role: Text) -> Text {
    if role == "none" {
        ""
    } else {
        let c = deck_colours(d)
        role_colour(c, role, c.surface)
    }
}

fn role_colour(c: Colours, role: Text, ground: Text) -> Text {
    if role.starts_with("#") {
        role
    } else if role == "accent-soft" {
        c.accent_soft
    } else if role == "accent-deep" {
        c.accent_deep
    } else if role == "surface" {
        c.surface
    } else if role == "surface-dim" {
        c.surface_dim
    } else if role == "deep" {
        c.deep
    } else if role == "text" {
        readable_on(ground, c.text, c.text_dim)
    } else if role == "text-dim" {
        readable_on(ground, c.text_dim, c.text)
    } else if role == "on-accent" {
        c.on_accent
    } else if role == "white" {
        "#FFFFFF"
    } else if role == "success" {
        c.success
    } else if role == "warning" {
        c.warning
    } else if role == "danger" { c.danger } else if role == "info" { c.info } else { c.accent }
}

/// `share` of the way from `from` to `to`, both `#RRGGBB`.
fn mix(from: Text, to: Text, share: Float) -> Text {
    hex_of(
        rgb(channel(from, 0), channel(from, 1), channel(from, 2)),
        rgb(channel(to, 0), channel(to, 1), channel(to, 2)),
        share,
    )
}

fn rgb(r: Int, g: Int, b: Int) -> List<Int> {
    [r, g, b]
}

fn r_of(c: List<Int>) -> Float {
    (c.at(0) ?? 0).to_float()
}

fn g_of(c: List<Int>) -> Float {
    (c.at(1) ?? 0).to_float()
}

fn b_of(c: List<Int>) -> Float {
    (c.at(2) ?? 0).to_float()
}

fn hex_of(from: List<Int>, to: List<Int>, share: Float) -> Text {
    "#"
        + hex_pair(blend(r_of(from), r_of(to), share))
        + hex_pair(blend(g_of(from), g_of(to), share))
        + hex_pair(blend(b_of(from), b_of(to), share))
}

fn blend(from: Float, to: Float, share: Float) -> Int {
    (from + (to - from) * share).round().clamp(0, 255)
}

fn hex_pair(n: Int) -> Text {
    hex_digit(n / 16) + hex_digit(n % 16)
}

fn hex_digit(n: Int) -> Text {
    if n <= 9 {
        "{n}"
    } else if n == 10 {
        "A"
    } else if n == 11 {
        "B"
    } else if n == 12 { "C" } else if n == 13 { "D" } else if n == 14 { "E" } else { "F" }
}

/// The red, green or blue channel of `#RRGGBB`.
fn channel(hex: Text, at: Int) -> Int {
    hex_value(hex.slice(1 + at * 2, 2 + at * 2)) * 16 + hex_value(hex.slice(2 + at * 2, 3 + at * 2))
}

fn hex_value(c: Text) -> Int {
    let lower = c.upper()
    if lower == "0" {
        0
    } else if lower == "1" {
        1
    } else if lower == "2" {
        2
    } else if lower == "3" {
        3
    } else if lower == "4" {
        4
    } else if lower == "5" {
        5
    } else if lower == "6" {
        6
    } else if lower == "7" {
        7
    } else if lower == "8" {
        8
    } else if lower == "9" {
        9
    } else if lower == "A" {
        10
    } else if lower == "B" {
        11
    } else if lower == "C" {
        12
    } else if lower == "D" { 13 } else if lower == "E" { 14 } else if lower == "F" { 15 } else { 0 }
}

/// `preferred` where it clears AA on `ground`, else whichever of `other` and white or near-black
/// reads better there.
fn readable_on(ground: Text, preferred: Text, other: Text) -> Text {
    if contrast(preferred, ground) >= 4.5 {
        preferred
    } else if contrast(other, ground) >= contrast("#FFFFFF", ground)
        and contrast(other, ground) >= contrast("#000000", ground) {
        other
    } else if contrast("#FFFFFF", ground) >= contrast("#000000", ground) {
        "#FFFFFF"
    } else { "#000000" }
}

/// The WCAG contrast ratio of two colours, from 1 to 21.
fn contrast(a: Text, b: Text) -> Float {
    let la = luminance(a)
    let lb = luminance(b)
    if la > lb { (la + 0.05) / (lb + 0.05) } else { (lb + 0.05) / (la + 0.05) }
}

fn luminance(c: Text) -> Float {
    0.2126 * linear(channel(c, 0).to_float())
        + 0.7152 * linear(channel(c, 1).to_float())
        + 0.0722 * linear(channel(c, 2).to_float())
}

fn linear(value: Float) -> Float {
    let v = value / 255.0
    if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).pow(2.4) }
}

/// A deck in the window: its slides in a rail, the slide being edited, the properties of what is
/// chosen, and the presentation the person runs from here.
// ---------------------------------------------------------------------------------------------
// The window: the slide rail, the stage and the properties beside it.
view deck(d: Deck) {
    state current: Text = ""
    state chosen: Text = "none"
    state presenting: Bool = false
    state presenter: Bool = false
    state notes_shown: Bool = false
    state blanked: Text = ""
    state overview: Bool = false
    state help: Bool = false
    state properties: Bool = true
    state audience_screen: Bool = false
    // How large the stage draws the slide (0 fits it to the window) and where the person moved it.
    state zoom: Float = 0.0
    state pan: List<Float> = [0.0, 0.0]
    // The person opened the chosen box for typing (Enter on it): the box takes the keyboard.
    state editing: Bool = false
    // A slide opened from search or from another app lands here, in the deck's own window: the
    // rail, the stage and the properties follow it, and the notes editor binds its notes.
    on show(s: Slide): set(current, s.id()).then(set(properties, true))
    // A box opened from search lands on the slide it stands on with the box chosen: the hit named
    // the box, so the person gets the box's properties and its words, not just its slide.
    on show(e: Element): if let s = box_slide(e) {
        set(current, s.id()).then(set(chosen, e.id())).then(set(properties, true))
    } else { set(chosen, "none") }
    let shown = current_slide(d, current)
    ui.Fullscreen(
        showing: presenting,
        name: "Presentation",
        screen: if audience_screen { .primary_external } else { .largest },
    ) {
        stack(fill) {
            if presenting {
                present_view(d, shown, current, presenter, notes_shown, blanked, audience_screen)
            } else if overview {
                overview_grid(d, current, overview)
            } else {
                ui.NavShell(content_kind: .page, title: deck_name(d), sidebar_title: "Slides") {
                    on drop(files: List<File>): import_files(d, files)
                    actions: {
                        // Search inside this deck: the same search, narrowed to its slides and
                        // their words, so a hit opens the slide it found here.
                        ui.SearchButton(item: d, label: deck_name(d))
                        ui.Button("Open", icon: .folder_open, key: "Ctrl+O") {
                            on press: file.choose(
                                import_file(d),
                                accept: [
                                    "application/vnd.oasis.opendocument.presentation",
                                    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                                ],
                            )
                        }
                        ui.Button("Add slide", icon: .plus, key: "n") {
                            on press: add_slide(d).then(set(current, "")).then(set(chosen, "none"))
                        }
                        if d.slides.len() > 0 {
                            ui.Button("Duplicate slide", icon: .copy, key: "Ctrl+D") {
                                on press: duplicate_current_slide(d, current)
                            }
                        }
                        if d.slides.len() > 0 {
                            ui.Button("Present", icon: .play, role: .primary, key: "p") {
                                on press: begin_presenting(d)
                                    .then(set(presenting, true))
                                    .then(set(presenter, false))
                                    .then(
                                        set(notes_shown, false),
                                    )
                                    .then(set(blanked, ""))
                            }
                        }
                        ui.Button(
                            "All slides",
                            icon: .layout_grid,
                            icon_only: true,
                            toggled: overview,
                            key: "g",
                        ) {
                            on press: set(overview, true)
                        }
                        ui.MenuButton(zoom_label(zoom), icon: .zoom_in) {
                            ui.MenuItem("Fit", icon: .maximize) {
                                on choose: set(zoom, 0.0).then(set(pan, [0.0, 0.0]))
                            }
                            for step in zoom_steps() {
                                let percent = (step * 100.0).round()
                                ui.MenuItem("{percent}%", icon: .zoom_in) {
                                    on choose: set(zoom, step).then(set(pan, [0.0, 0.0]))
                                }
                            }
                        }
                        ui.Button(
                            "Properties",
                            icon: .panel_right,
                            icon_only: true,
                            toggled: properties,
                        ) {
                            on press: set(properties, not properties)
                        }
                        ui.Button("Help", icon: .circle_help, icon_only: true, key: "Shift+?") {
                            on press: set(help, true)
                        }
                    }
                    sidebar: {
                        slide_rail(d, current, chosen)
                    }
                    content: {
                        stack(gap: .related, fill) {
                            if d.notice != "" {
                                ui.Banner(d.notice, kind: .warning) {
                                    on act: clear_notice(d)
                                }
                            }
                            if let s = shown {
                                stack(gap: .related, padding: .related, fill) {
                                    slide_stage(d, s, chosen, zoom, pan, editing)
                                    insert_tools(s, chosen, editing)
                                    // A compact window keeps the speaker's words under the
                                    // slide they are said over, where the inspector's sheet
                                    // would cover it.
                                    if space.compact {
                                        stack(gap: .tight) {
                                            ui.Text(
                                                "Speaker notes",
                                                role: .caption,
                                                secondary: true,
                                            )
                                            ui.BlockEditor(bind: s.notes, placeholder: "Notes")
                                        }
                                    }
                                }
                            } else {
                                ui.EmptyState(
                                    "No slides yet",
                                    message: "Add a slide to start the deck",
                                    icon: .monitor,
                                )
                            }
                        }
                    }
                    inspector: {
                        if properties {
                            if let s = shown {
                                inspector(d, s, chosen)
                            }
                        }
                    }
                }
            }
            if help {
                help_sheet(help)
            }
        }
    }
}

/// How much the stage is zoomed, as the bar names it: fitted, or the size it draws a slide at.
fn zoom_label(zoom: Float) -> Text {
    if zoom <= 0.0 {
        return "Zoom: Fit"
    }
    let percent = (zoom * 100.0).round()
    "Zoom: {percent}%"
}

/// The sizes the zoom menu steps between: a slide drawn at its own pixels reads at 100%, and the
/// steps around it hold the sizes a person picks to see more of the slide or more of the page.
fn zoom_steps() -> List<Float> {
    [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0]
}

/// The deck's name, or the word a deck with no name shows.
fn deck_name(d: Deck) -> Text {
    if d.title.trim() == "" { "Untitled deck" } else { d.title }
}

/// A slide on its own, for a search that found its words: the slide as it is drawn, and what the
/// speaker says over it. Its type's default view, so a hit and an agent's `open` land in a
/// window that shows the slide when the deck's own window is not the one taking it.
view slide_page(s: Slide) {
    let d = root Deck
    ui.NavShell(content_kind: .page, title: slide_title(s, d.slides.index_of(s) ?? 0)) {
        content: {
            ui.Scroll {
                stack(gap: .group, padding: .related) {
                    slide_picture(d, s, "Slide", .none)
                    ui.Text("Speaker notes", role: .caption, secondary: true)
                    ui.BlockEditor(bind: s.notes, placeholder: "What you say over this slide")
                }
            }
        }
    }
}

/// A box on its own, for a search that found its words in it and for an agent that opens it:
/// the slide it stands on with the box chosen, and the words it holds. The deck's window answers
/// showing a box in place, so this page is what a window that cannot shows it through.
view box_page(e: Element) {
    let d = root Deck
    if let s = box_slide(e) {
        ui.NavShell(content_kind: .page, title: slide_title(s, d.slides.index_of(s) ?? 0)) {
            content: {
                ui.Scroll {
                    stack(gap: .group, padding: .related) {
                        slide_picture(d, s, "Slide", .none)
                        ui.Text("The box's words", role: .caption, secondary: true)
                        ui.BlockEditor(bind: e.body, placeholder: "Add text", size: e.font_size)
                    }
                }
            }
        }
    } else {
        ui.EmptyState(
            "This box's slide is gone",
            message: "The slide it stood on is no longer in the deck",
            icon: .type,
        )
    }
}

/// The rail: the slides as small live pictures of themselves, a press opening one, and its place
/// among the others changed by the buttons beside it.
view slide_rail(d: Deck, current: State<Text>, chosen: State<Text>) {
    ui.VirtualList(fill, chosen_values: [current]) {
        on select(picked: Slide): set(current, picked.id()).then(set(chosen, "none"))
        on reorder(ids: List<Text>): reorder_slides(d, ids)
        for s in d.slides {
            let index = d.slides.index_of(s) ?? 0
            ui.ListItem(item: s) {
                menu: {
                    ui.MenuItem("Open", icon: .external_link) {
                        on choose: set(current, s.id()).then(set(chosen, "none"))
                    }
                    ui.MenuItem("Duplicate", icon: .copy) {
                        on choose: duplicate_slide(d, s)
                    }
                    ui.MenuItem("Move the slide up", icon: .arrow_up) {
                        on choose: move_slide(d, s, (index - 1).max(0))
                    }
                    ui.MenuItem("Move the slide down", icon: .arrow_down) {
                        on choose: move_slide(d, s, (index + 1).min(d.slides.len() - 1))
                    }
                    ui.MenuItem("Delete", icon: .trash, destructive: true) {
                        on choose: delete_slide(d, s)
                    }
                }
                row(gap: .related, align: .center) {
                    ui.Text("{index + 1}", role: .caption, secondary: true, numeric: true)
                    ui.Stage(
                        canvas: [1280, 720],
                        height: 62.0,
                        aspect: 16 / 9,
                        background: slide_ground(d, s),
                        name: "Slide {index + 1}",
                    ) {
                        for e in s.elements {
                            ui.StagePin(
                                name: element_label(e),
                                item: e.id(),
                                x: e.x,
                                y: e.y,
                                width: e.w,
                                height: e.h,
                            ) {
                                element_view(d, s, e, "", false, 0.1)
                            }
                        }
                    }
                    ui.Button("Move the slide up", icon: .arrow_up, icon_only: true, role: .plain) {
                        on press: move_slide(d, s, (index - 1).max(0))
                    }
                    ui.Button(
                        "Move the slide down",
                        icon: .arrow_down,
                        icon_only: true,
                        role: .plain,
                    ) {
                        on press: move_slide(d, s, (index + 1).min(d.slides.len() - 1))
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

/// The stage: the slide's elements at their places, as large as the window takes, the person
/// choosing, moving and sizing them there.
view slide_stage(
    d: Deck,
    s: Slide,
    chosen: State<Text>,
    zoom: State<Float>,
    pan: State<List<Float>>,
    editing: State<Bool>,
) {
    ui.Stage(
        name: "Slide",
        canvas: [1280, 720],
        background: slide_ground(d, s),
        chosen: chosen_key(s, chosen),
        zoom: if zoom > 0.0 { .some(zoom) } else { .none },
        pan: .some(pan),
        fill,
    ) {
        on press: set(chosen, "none").then(set(editing, false))
        on zoomed(scale: Float): set(zoom, scale)
        on panned(at: List<Float>): set(pan, at)
        on select(target: Text): set(chosen, target).then(set(editing, opens_empty_box(s, target)))
        on open(target: Text): set(chosen, target).then(set(editing, true))
        on delete(target: Text): delete_element_at(s, target).then(set(chosen, "none"))
        on moved(target: Text, at: List<Int>): move_element_to(s, target, at)
        on resized(target: Text, at: List<Int>): resize_element_to(s, target, at)
        on context(target: Text): set(chosen, target)
        for e in s.elements {
            ui.StagePin(
                name: element_label(e),
                item: e.id(),
                x: e.x,
                y: e.y,
                width: e.w,
                height: e.h,
                moves: true,
            ) {
                menu: {
                    ui.MenuItem("Bring to front", icon: .bring_to_front, key: "]") {
                        on choose: bring_to_front(s, e)
                    }
                    ui.MenuItem("Send to back", icon: .send_to_back, key: "[") {
                        on choose: send_to_back(s, e)
                    }
                    ui.MenuItem("Duplicate", icon: .layers, key: "d") {
                        on choose: duplicate_element(s, e).then(set(chosen, ""))
                    }
                    ui.MenuItem("Copy", icon: .copy, key: "Ctrl+C") {
                        on choose: copy_element(e)
                    }
                    ui.MenuItem("Cut", icon: .scissors, key: "Ctrl+X") {
                        on choose: cut_element(s, e)
                    }
                    ui.MenuItem("Paste", icon: .clipboard, key: "Ctrl+V") {
                        on choose: paste_element(s)
                    }
                    ui.MenuItem("Delete", icon: .trash, destructive: true, key: "Delete") {
                        on choose: delete_element(s, e).then(set(chosen, "none"))
                    }
                }
                element_view(d, s, e, chosen, editing, 1.0)
            }
        }
    }
}

/// One element as it is drawn on the slide: its words, its picture, or its shape.
view element_view(d: Deck, s: Slide, e: Element, chosen: Text, editing: Bool, scale: Float) {
    if e.kind == .text {
        if e.id() == chosen {
            ui.BlockEditor(
                bind: e.body,
                placeholder: "Add text",
                size: e.font_size,
                align: align_code(e.align),
                colour: slide_ink(d, s, e.fill),
                focus: editing,
            )
        } else {
            ui.BlockEditor(
                e.body,
                placeholder: "Add text",
                size: e.font_size,
                align: align_code(e.align),
                colour: slide_ink(d, s, e.fill),
            )
        }
    } else if e.kind == .image {
        if e.src != "" {
            ui.Image(e.src, alt: element_label(e), fit: .cover)
        } else {
            ui.EmptyState("Picture", message: "Choose a picture", icon: .image)
        }
    } else if e.kind == .line {
        ui.Drawing(
            [line_of(d, e)],
            meaning: .decoration,
            width: e.w.to_float(),
            height: if e.h < 16 { 16.0 } else { e.h.to_float() },
        )
    } else if e.kind == .ellipse {
        ui.Drawing(
            [oval_of(d, e)],
            meaning: .decoration,
            width: e.w.to_float(),
            height: e.h.to_float(),
        )
    } else {
        ui.Drawing(
            [rect_of(d, e)],
            meaning: .decoration,
            width: e.w.to_float(),
            height: e.h.to_float(),
        )
    }
}

/// The rectangle an element draws: its fill, its outline and its rounded corners, in the deck's
/// colours, over its own box.
fn rect_of(d: Deck, e: Element) -> ui.Shape {
    let side = e.w.min(e.h).to_float().max(1.0)
    ui.Shape.area(
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
        round: e.radius / side,
        colour: role_fill(d, e.fill),
        stroke_colour: role_fill(d, e.stroke),
        stroke_width: e.stroke_width / side,
    )
}

/// The ellipse an element draws.
fn oval_of(d: Deck, e: Element) -> ui.Shape {
    let side = e.w.min(e.h).to_float().max(1.0)
    ui.Shape.oval(
        cx: 0.5,
        cy: 0.5,
        rx: 0.5,
        ry: 0.5,
        colour: role_fill(d, e.fill),
        stroke_colour: role_fill(d, e.stroke),
        stroke_width: e.stroke_width / side,
    )
}

/// A line across an element's box.
fn line_of(d: Deck, e: Element) -> ui.Shape {
    let side = e.w.min(e.h).to_float().max(1.0)
    ui.Shape.stroke(
        points: [0.0, 0.5, 1.0, 0.5],
        colour: role_fill(d, e.stroke),
        width: e.stroke_width / side,
    )
}

// ---------------------------------------------------------------------------------------------
// The properties of what is chosen, and of the slide beside it.
view inspector(d: Deck, s: Slide, chosen: State<Text>) {
    stack(padding: .related, gap: .related) {
        ui.Text("Properties", role: .subheading)
        if let e = element_in(s, chosen_key(s, chosen)) {
            ui.Text(element_label(e), role: .body)
            ui.Text("Fill", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for role in [
                    "accent",
                    "accent-soft",
                    "surface",
                    "surface-dim",
                    "deep",
                    "text",
                    "text-dim",
                    "none",
                    "success",
                    "warning",
                    "danger",
                    "info",
                ] {
                    ui.Button(role, role: .plain, toggled: e.fill == role) {
                        on press: set_style(e, fill: .some(role))
                    }
                }
            }
            ui.ColorPicker(
                bind: e.fill,
                label: "Fill colour",
                swatches: [
                    "#4F46E5",
                    "#10B981",
                    "#F59E0B",
                    "#EC4899",
                    "#0EA5E9",
                    "#0F172A",
                    "#FFFFFF",
                ],
            )
            ui.Text("Outline", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for role in ["none", "text", "accent", "success", "warning", "danger", "info"] {
                    ui.Button(role, role: .plain, toggled: e.stroke == role) {
                        on press: set_style(
                            e,
                            stroke: .some(role),
                            stroke_width: .some(if role == "none" { 0.0 } else { 4.0 }),
                        )
                    }
                }
            }
            if e.kind == .text {
                ui.Text("Font", role: .caption, secondary: true)
                row(gap: .tight, align: .center) {
                    ui.NumberInput(
                        bind: e.font_size,
                        label: "Font size",
                        min: 8.0,
                        max: 160.0,
                        step: 2.0,
                        suffix: "pt",
                    )
                    ui.Button("Bold", icon: .bold, icon_only: true, toggled: e.bold) {
                        on press: set_style(e, bold: .some(not e.bold))
                    }
                    ui.Button("Italic", icon: .italic, icon_only: true, toggled: e.italic) {
                        on press: set_style(e, italic: .some(not e.italic))
                    }
                }
                ui.Text("Alignment", role: .caption, secondary: true)
                row(gap: .tight) {
                    for align in [Align.left, .center, .right, .justify] {
                        ui.Button(align.label(), toggled: e.align == align) {
                            on press: set_style(e, align: .some(align))
                        }
                    }
                }
            }
            ui.Text("Arrange", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                ui.Button("Bring front", icon: .bring_to_front, key: "]") {
                    on press: bring_to_front(s, e)
                }
                ui.Button("Send back", icon: .send_to_back, key: "[") {
                    on press: send_to_back(s, e)
                }
                ui.Button("Nudge left", icon: .arrow_left, icon_only: true) {
                    on press: nudge_element_to(s, e.id(), -12, 0)
                }
                ui.Button("Nudge right", icon: .arrow_right, icon_only: true) {
                    on press: nudge_element_to(s, e.id(), 12, 0)
                }
                ui.Button("Nudge up", icon: .arrow_up, icon_only: true) {
                    on press: nudge_element_to(s, e.id(), 0, -12)
                }
                ui.Button("Nudge down", icon: .arrow_down, icon_only: true) {
                    on press: nudge_element_to(s, e.id(), 0, 12)
                }
                ui.Button("Previous object", icon: .skip_back, icon_only: true) {
                    on press: set(chosen, previous_element_key(s, chosen_key(s, chosen)))
                }
                ui.Button("Next object", icon: .skip_forward, icon_only: true) {
                    on press: set(chosen, next_element_key(s, chosen_key(s, chosen)))
                }
                ui.Button("Duplicate", icon: .layers, key: "d") {
                    on press: duplicate_element(s, e).then(set(chosen, ""))
                }
                ui.Button("Copy", icon: .copy, key: "Ctrl+C") {
                    on press: copy_element(e)
                }
                ui.Button("Cut", icon: .scissors, key: "Ctrl+X") {
                    on press: set(chosen, "none").then(cut_element(s, e))
                }
                ui.Button("Delete", icon: .trash, role: .destructive, key: "Delete") {
                    on press: set(chosen, "").then(delete_element(s, e))
                }
            }
        } else {
            ui.Text(slide_title(s, 0), role: .body)
            ui.Text("Layout", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for layout in [Layout.title, .title_content, .two_column, .section, .blank] {
                    ui.Button(layout.label(), toggled: s.layout == layout) {
                        on press: set_layout(s, layout)
                    }
                }
            }
            ui.Text("Background", role: .caption, secondary: true)
            row(gap: .tight) {
                for background in [Background.surface, .accent, .deep] {
                    ui.Button(background.label(), toggled: s.background == background) {
                        on press: set_background(s, background)
                    }
                }
            }
            ui.Text("Deck look", role: .caption, secondary: true)
            row(gap: .tight) {
                for look in [Look.light, .dark] {
                    ui.Button(look.label(), toggled: d.look == look) {
                        on press: set_look(d, look)
                    }
                }
            }
            ui.Text("Deck theme", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for palette in [Palette.indigo, .emerald, .amber, .rose, .sky] {
                    ui.Button(palette.label(), toggled: d.palette == palette) {
                        on press: set_palette(d, palette)
                    }
                }
            }
        }
        ui.Text("Speaker notes", role: .caption, secondary: true)
        ui.BlockEditor(bind: s.notes, placeholder: "What you say over this slide")
        ui.Text("Deck", role: .caption, secondary: true)
        ui.TextInput(bind: d.title, placeholder: "Presentation title")
        row(gap: .tight, wrap: true) {
            ui.Button("Export as PowerPoint", icon: .download, key: "Ctrl+S") {
                on press: file.save(export_pptx(d), name: export_name(d, "pptx"))
            }
            ui.Button("Export as OpenDocument", icon: .download) {
                on press: file.save(export_odp(d), name: export_name(d, "odp"))
            }
        }
    }
}

/// The tools that put a text box, a shape, a line or a picture on the slide.
view insert_tools(s: Slide, chosen: State<Text>, editing: State<Bool>) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.Text("Insert", role: .caption, secondary: true)
        ui.Button("Text", icon: .type, key: "t") {
            // The box the press made is the one the person types in: the binding holds what
            // the action answered, so the next key lands in it.
            on press: {
                let made = add_element(s, .text, x: 440, y: 320)
                set(chosen, made.id()).then(set(editing, true))
            }
        } as insert_text
        ui.Button("Rectangle", icon: .square, key: "r") {
            on press: {
                let made = add_element(s, .rect, x: 480, y: 260)
                set(chosen, made.id()).then(set(editing, false))
            }
        }
        ui.Button("Ellipse", icon: .circle, key: "o") {
            on press: {
                let made = add_element(s, .ellipse, x: 480, y: 260)
                set(chosen, made.id()).then(set(editing, false))
            }
        }
        ui.Button("Line", icon: .minus, key: "l") {
            on press: {
                let made = add_element(s, .line, x: 440, y: 360)
                set(chosen, made.id()).then(set(editing, false))
            }
        }
        ui.Button("Picture", icon: .image) {
            on press: file.choose(
                add_picture(s),
                accept: [
                    "image/png",
                    "image/jpeg",
                    "image/webp",
                    "image/gif",
                    "image/bmp",
                    "image/svg+xml",
                ],
            )
        }
        ui.Button("Paste", icon: .clipboard, icon_only: true, key: "Ctrl+V") {
            on press: paste_element(s).then(set(chosen, ""))
        }
        if let e = element_in(s, chosen_key(s, chosen)) {
            ui.Button("Copy", icon: .copy, icon_only: true, key: "Ctrl+C") {
                on press: copy_element(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The presentation: the slide filling the screen, and the speaker's controls over it.
view present_view(
    d: Deck,
    shown: Option<Slide>,
    current: State<Text>,
    presenter: State<Bool>,
    notes_shown: State<Bool>,
    blanked: State<Text>,
    audience_screen: State<Bool>,
) {
    stack(fill) {
        if blanked != "" {
            ui.Stage(
                name: "Blank",
                canvas: [1280, 720],
                background: if blanked == "black" { "#000000" } else { "#FFFFFF" },
                fill,
            ) {
            }
        } else if presenter {
            presenter_view(d, shown, current, notes_shown)
        } else if let s = shown {
            ui.Stage(
                name: "Presentation",
                canvas: [1280, 720],
                background: slide_ground(d, s),
                fill,
            ) {
                for e in s.elements {
                    ui.StagePin(
                        name: element_label(e),
                        item: e.id(),
                        x: e.x,
                        y: e.y,
                        width: e.w,
                        height: e.h,
                    ) {
                        element_view(d, s, e, "", false, 1.0)
                    }
                }
            }
            if notes_shown {
                ui.Card(title: "Speaker notes") {
                    ui.BlockEditor(s.notes)
                }
            }
        } else {
            ui.EmptyState("End of the presentation", icon: .monitor)
        }
        row(gap: .tight, align: .center, wrap: true) {
            ui.Button("Previous", icon: .chevron_left, key: "Left") {
                on press: set(
                    current,
                    if let p = previous_slide(d, current) { p.id() } else { current },
                )
            }
            ui.Button("Next", icon: .chevron_right, key: "Right", keys: ["Space"]) {
                on press: set(
                    current,
                    if let n = next_slide(d, current) { n.id() } else { current },
                )
            }
            ui.Button("First", icon: .skip_back, key: "Home") {
                on press: set(
                    current,
                    if let first = d.slides.at(0) { first.id() } else { current },
                )
            }
            ui.Button("Last", icon: .skip_forward, key: "End") {
                on press: set(
                    current,
                    if let last = d.slides.at(d.slides.len() - 1) { last.id() } else { current },
                )
            }
            if d.clock_paused {
                ui.Text(clock_text(d.clock_held), role: .caption, numeric: true)
            } else if let since = d.presenting_since {
                ui.Countdown(at: since, size: .small)
            }
            ui.Button(
                "Pause the clock",
                icon: .pause,
                icon_only: true,
                toggled: d.clock_paused,
                key: "k",
            ) {
                on press: if d.clock_paused { resume_clock(d) } else { pause_clock(d) }
            }
            ui.Button("Reset the clock", icon: .rotate_ccw, key: "0") {
                on press: begin_presenting(d)
            }
            ui.Button("Notes", icon: .notebook, toggled: notes_shown, key: "s") {
                on press: set(notes_shown, not notes_shown)
            }
            ui.Button("Presenter", icon: .panel_right, toggled: presenter, key: "p") {
                on press: set(presenter, not presenter)
            }
            // The button's own name says what it picks: the screen someone plugged in, rather
            // than the largest external one.
            ui.Button(
                "External screen",
                icon: .monitor,
                icon_only: true,
                toggled: audience_screen,
            ) {
                on press: set(audience_screen, not audience_screen)
            }
            ui.Button("Black", icon: .moon, key: "b") {
                on press: set(blanked, if blanked == "black" { "" } else { "black" })
            }
            ui.Button("White", icon: .sun, key: "w") {
                on press: set(blanked, if blanked == "white" { "" } else { "white" })
            }
            ui.Button("End", icon: .square, role: .primary, key: "Escape") {
                on press: end_presenting(d).then(set(blanked, ""))
            }
            for n in [1, 2, 3, 4, 5, 6, 7, 8, 9] {
                if let target = d.slides.at(n - 1) {
                    ui.Button("{n}", role: .plain, key: "{n}") {
                        on press: set(current, target.id())
                    }
                }
            }
        }
    }
}

/// A length of time as a speaker's clock reads it, minutes and seconds.
fn clock_text(seconds: Int) -> Text {
    let left = seconds.max(0)
    let minutes = left / 60
    let rest = left % 60
    if rest < 10 { "{minutes}:0{rest}" } else { "{minutes}:{rest}" }
}

/// The presenter's view: the slide being shown, the next one, and what the speaker says.
view presenter_view(d: Deck, shown: Option<Slide>, current: State<Text>, notes_shown: State<Bool>) {
    split(side: .trailing, collapse: .stack) {
        primary(min: 24rem) {
            stack(gap: .related) {
                if let s = shown {
                    ui.Text(
                        "Slide {(d.slides.index_of(s) ?? 0) + 1} of {d.slides.len()}",
                        role: .subheading,
                    )
                    slide_picture(d, s, "Current", .none)
                }
            }
        }
        secondary(min: 16rem) {
            stack(gap: .related) {
                if let next = next_slide(d, current) {
                    ui.Text("Next", role: .subheading)
                    slide_picture(d, next, "Next slide", .some(216.0))
                } else {
                    ui.Text("End of the presentation", role: .body, secondary: true)
                }
                ui.Text("Speaker notes", role: .subheading)
                if let s = shown {
                    if s.notes.is_empty() {
                        ui.Text("No notes for this slide.", role: .body, secondary: true)
                    } else {
                        ui.BlockEditor(s.notes)
                    }
                }
                if d.clock_paused {
                    ui.Text("Paused at {clock_text(d.clock_held)}", role: .caption, secondary: true)
                } else if let since = d.presenting_since {
                    ui.Countdown(at: since, label: "Elapsed", size: .small)
                }
            }
        }
    }
}

/// A slide as a picture of itself: what the rail, the presenter and the overview show.
view slide_picture(d: Deck, s: Slide, name: Text, height: Option<Float>) {
    ui.Stage(
        name: name,
        canvas: [1280, 720],
        background: slide_ground(d, s),
        height: height,
        aspect: 16 / 9,
    ) {
        for e in s.elements {
            ui.StagePin(
                name: element_label(e),
                item: e.id(),
                x: e.x,
                y: e.y,
                width: e.w,
                height: e.h,
            ) {
                element_view(d, s, e, "", false, 1.0)
            }
        }
    }
}

/// Every slide at once, each a picture opening it.
view overview_grid(d: Deck, current: State<Text>, overview: State<Bool>) {
    stack(gap: .related, padding: .related, fill) {
        row(gap: .related, align: .center) {
            ui.Text("All slides", role: .subheading)
            ui.Button("Back to the editor", icon: .arrow_left, key: "g") {
                on press: set(overview, false)
            }
        }
        scroll(fill) {
            grid(min: 16rem, gap: .related) {
                for s in d.slides {
                    let index = d.slides.index_of(s) ?? 0
                    ui.Card(
                        title: "{index + 1}. {slide_title(s, index)}",
                        item: s,
                        selected: s.id() == current,
                    ) {
                        on activate: set(current, s.id()).then(set(overview, false))
                        slide_picture(d, s, "Slide {index + 1}", .some(162.0))
                    }
                }
            }
        }
    }
}

/// The keyboard reference: what a press does in the editor and in a presentation.
view help_sheet(help: State<Bool>) {
    ui.ModalDialog(
        title: "Keyboard shortcuts",
        icon: .circle_help,
        open: help,
        secondary: "Close",
    ) {
        on act_secondary: set(help, false)
        on dismiss: set(help, false)
        stack(gap: .related, padding: .related) {
            ui.Text("Editing", role: .heading)
            ui.KeyHints {
                ui.KeyHint(["N"], "Add a slide")
                ui.KeyHint(["P"], "Present")
                ui.KeyHint(["G"], "All slides")
                ui.KeyHint(["T, R, O, L"], "Insert a text box, rectangle, ellipse or line")
                ui.KeyHint(["Ctrl+O"], "Open a presentation")
                ui.KeyHint(["Ctrl+S"], "Export the deck")
                ui.KeyHint(["Ctrl+D"], "Duplicate the slide")
                ui.KeyHint(["D"], "Duplicate the chosen object")
                ui.KeyHint(["Delete"], "Delete the chosen object")
                ui.KeyHint(["[ and ]"], "Send back or bring front")
                ui.KeyHint(["Ctrl+C, Ctrl+X, Ctrl+V"], "Copy, cut, paste an object")
                ui.KeyHint(["Drag"], "Move an object")
                ui.KeyHint(["Drag an edge"], "Size an object")
                ui.KeyHint(["?", "Esc"], "Close this reference")
            }
            ui.Text("Presenting", role: .heading)
            ui.KeyHints {
                ui.KeyHint(["Left, Right"], "Previous or next slide")
                ui.KeyHint(["Home, End"], "First or last slide")
                ui.KeyHint(["1 to 9"], "Go to a slide")
                ui.KeyHint(["S"], "Speaker notes")
                ui.KeyHint(["P"], "Presenter view")
                ui.KeyHint(["B, W"], "Black or white screen")
                ui.KeyHint(["K, 0"], "Pause or reset the clock")
                ui.KeyHint(["Esc"], "End the presentation")
            }
        }
    }
}

/// Where the presentation currently stands.
fn current_slide(d: Deck, current: Text) -> Option<Slide> {
    if let s = d.slides.find(x -> x.id() == current) { .some(s) } else { d.slides.at(0) }
}

/// The next slide, wrapping past the last.
fn next_slide(d: Deck, current: Text) -> Option<Slide> {
    let at = if let s = current_slide(d, current) { (d.slides.index_of(s) ?? 0) + 1 } else { 0 }
    if let after = d.slides.at(at) { .some(after) } else { d.slides.at(0) }
}

/// The slide before, wrapping past the first.
fn previous_slide(d: Deck, current: Text) -> Option<Slide> {
    let at = if let s = current_slide(d, current) { d.slides.index_of(s) ?? 0 } else { 0 }
    if at > 0 { d.slides.at(at - 1) } else { d.slides.at(d.slides.len() - 1) }
}

/// The element of a slide with this key, if it is there.
fn element_in(s: Slide, key: Text) -> Option<Element> {
    s.elements.find(x -> x.id() == key)
}

/// Whether the click that chose `target` also opens it for typing: a text box with no words yet
/// is one a person means to write in, so one click puts the caret in it. A box that has words is
/// chosen by the first click; the next one, or Enter, opens it at the word pressed.
fn opens_empty_box(s: Slide, target: Text) -> Bool {
    if let e = element_in(s, target) { e.kind == .text and e.body.is_empty() } else { false }
}

/// The slide the box `e` stands on, when the deck still holds it.
fn box_slide(e: Element) -> Option<Slide> {
    let d = root Deck
    for s in d.slides {
        if s.elements.any(x -> x.id() == e.id()) {
            return .some(s)
        }
    }
    .none
}

/// The element after `key` on the slide, around to the first.
fn next_element_key(s: Slide, key: Text) -> Text {
    let at = if let e = s.elements.find(x -> x.id() == key) {
        (s.elements.index_of(e) ?? 0) + 1
    } else { 0 }
    if let e = s.elements.at(at) {
        e.id()
    } else if let first = s.elements.at(0) { first.id() } else { "" }
}

/// The element before `key` on the slide, around to the last.
fn previous_element_key(s: Slide, key: Text) -> Text {
    let at = if let e = s.elements.find(x -> x.id() == key) {
        s.elements.index_of(e) ?? 0
    } else { 0 }
    if at > 0 {
        if let before = s.elements.at(at - 1) { before.id() } else { "" }
    } else if let last = s.elements.last() { last.id() } else { "" }
}

/// The element the person is working on: the key the view chose, the element just made while
/// that key is empty, and none once they pressed the slide's empty surface.
fn chosen_key(s: Slide, chosen: Text) -> Text {
    if chosen == "none" {
        ""
    } else if chosen == "" { s.elements.last()?.id() ?? "" } else { chosen }
}
