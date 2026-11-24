app so.tilo.canvas "Canvas" (
    icon: .image,
    category: .graphics,
    tagline: "Draw and paint that stays editable",
    keywords: ["draw", "drawing", "paint", "sketch", "whiteboard", "art", "illustration", "canvas", "vector", "watercolour", "oil", "zeichnen", "malen", "skizze", "zeichenbrett", "leinwand", "tafel"],
)

/// An artwork: layers of marks, each a vector object with a stable id, over a ground. The
/// records are the work; the device's drawing engine is asked only to see it and to carry it
/// over its file, so every member of the workspace holds the same artwork and edits it alike.
type Artwork (title: title, untitled: "Untitled artwork") {
    title: Text = "Untitled artwork"
    width: Float = 1024.0
    height: Float = 768.0
    /// `smooth`, `paper`, `cold-press`, `rough`, `linen` or `panel`.
    ground_texture: Text = "paper"
    ground_colour: Text = "#f4efe3"
    /// Bottom to top.
    layers: Collection<Layer>
    /// Every mark of every layer; a mark names its layer.
    marks: Collection<Mark>
    /// Pictures placed in the artwork, by the address their bytes are kept at.
    pictures: Collection<Picture>
    /// The area selected for the next edit, kept with the artwork.
    selection: Option<Area>
    /// The picture of the artwork as last drawn, so the surface shows paint under the hand.
    look: Text = ""
    /// Why the last thing asked of the engine did not happen; empty when it did.
    notice: Text = ""

    search {
        sort title a_z
    }
}

/// One layer of the artwork: its own paint and what it lets through.
type Layer (title: name) {
    name: Text
    /// `oil`, `watercolour` or another medium saved in the artwork; empty for plain vector paint.
    medium: Text = ""
    opacity: Float = 1.0
    visible: Bool = true
    locked: Bool = false
    /// How much of the ground's tooth shows through its marks, 0 to 1.
    texture: Float = 0.0
}

/// One mark: a brush stroke (`stroke`) or a shape (`path`) drawn on a layer.
type Mark (title: name) {
    name: Text = ""
    layer: Layer
    /// `stroke` or `path`.
    kind: Text = "stroke"
    /// A stroke's points, `x, y, pressure` in turn, in artwork units.
    points: List<Float>
    /// A shape's SVG path data, or the area it is kept inside.
    d: Text = ""
    size: Float = 6.0
    colour: Text = "#1d1b19"
    /// `round`, `pen`, `pencil`, `marker`, `airbrush`, `charcoal`, `calligraphy`, `eraser`,
    /// `pixel` or `pixel-eraser`; empty for the round ink brush.
    brush: Text = ""
    opacity: Float = 1.0
    /// Whether the hand is still on it: the stroke a moving pointer extends. A stroke whose end
    /// never came (the window closed midway) stops being extended once the next one starts.
    open: Bool = false
    /// A picture's bytes, by the address they are kept at, and the frame it is drawn in.
    src: Text = ""
    x: Float = 0.0
    y: Float = 0.0
    width: Float = 0.0
    height: Float = 0.0
}

/// The area selected for the next edit.
type Area {
    /// SVG path data in artwork units.
    d: Text
    /// How far its edge fades, in units.
    feather: Float = 0.0
}

/// A picture the artwork shows, kept by the address of its bytes.
type Picture (title: name) {
    name: Text
    url: Text
    width: Int = 0
    height: Int = 0
}

/// The workspace's artworks.
type Library {
    artworks: Collection<Artwork>
}

/// What a look asks the device's drawing engine for.
type LookRequest {
    size: Int
    format: Text
}

/// What an export asks the device's drawing engine for.
type ExportRequest {
    format: Text
    scale: Float
}

/// What an SVG export asks for: the whole artwork, its text as text.
type SvgRequest {
    ids: Option<List<Text>>
    outline_text: Option<Bool>
}

/// Where an imported SVG goes: the top layer, in a group of its own.
type PlaceRequest {
    layer: Option<Text>
    group: Option<Text>
    fit: Option<List<Float>>
}

/// The artwork as the device's drawing engine reads it: its fields, and each mark with the
/// layer it stands on.
type ArtworkJson {
    title: Text
    width: Float
    height: Float
    ground_texture: Text
    ground_colour: Text
    layers: List<LayerJson>
    marks: List<MarkJson>
    selection: Option<AreaJson>
}

type LayerJson {
    name: Text
    medium: Text
    opacity: Float
    visible: Bool
    locked: Bool
    texture: Float
}

type MarkJson {
    name: Text
    layer: LayerJson
    kind: Text
    points: List<Float>
    d: Text
    size: Float
    colour: Text
    brush: Text
    opacity: Float
    src: Text
    x: Float
    y: Float
    width: Float
    height: Float
}

type AreaJson {
    d: Text
    feather: Float
}

root library: Library

default view library for Library

view library(lib: Library) {
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            ui.Button("New artwork", icon: .plus) {
                on press: new_artwork(lib, "Untitled artwork", 1024.0, 768.0)
            }
            ui.Button("Open an artwork…", icon: .file_plus) {
                on press: file.choose(
                    open_artwork(lib),
                    accept: ["application/vnd.tilo.artwork+zip", ".artwork"],
                )
            }
        }
        if lib.artworks.is_empty() {
            ui.EmptyState("No artwork yet", message: "New artwork starts one", icon: .image)
        } else {
            stack {
                for art in lib.artworks {
                    ui.ListRow(title: art.title, icon: .image, item: art)
                }
            }
        }
    }
}

default view artwork for Artwork

/// Opens a saved artwork from the file a person, Files or another app hands over.
primary action open_artwork(lib: Library, file: File) -> Artwork (
    icon: .file_plus,
    label: "Open artwork",
    opens: ["application/vnd.tilo.artwork+zip", .artwork],
    did: "opened an artwork",
) {
    match canvas.open(file) {
        .ok(text) => match json.parse(text) {
            .ok(opened) => {
                let texture = nested_text(opened, "ground", "texture")
                let ground = nested_text(opened, "ground", "colour")
                let made = lib.artworks.insert(
                    Artwork(
                        title: text_field(opened, "title"),
                        width: number_field(opened, "width") ?? 1024.0,
                        height: number_field(opened, "height") ?? 768.0,
                        ground_texture: if texture == "" { "paper" } else { texture },
                        ground_colour: if ground == "" { "#f4efe3" } else { ground },
                    ),
                )
                // Each layer keeps the name the file gave it, and the file's own layer id names
                // it for the marks that stood on it: the records take fresh ids of their own.
                let by_id: Map<Layer> = Map<Layer>()
                for row in array_field(opened, "layers") {
                    let name = text_field(row, "name")
                    let id = text_field(row, "id")
                    let layer = made.layers.insert(
                        Layer(
                            name: name,
                            medium: text_field(row, "medium"),
                            opacity: number_field(row, "opacity") ?? 1.0,
                            visible: flag_field(row, "visible") ?? true,
                            locked: flag_field(row, "locked") ?? false,
                            texture: number_field(row, "texture") ?? 0.0,
                        ),
                    )
                    by_id.put(if id == "" { name } else { id }, layer)
                }
                if made.layers.is_empty() {
                    made.layers.insert(Layer(name: "Layer 1"))
                }
                for row in array_field(opened, "marks") {
                    if let layer = by_id.get(text_field(row, "layer")) {
                        if let mark = mark_of(layer, row) {
                            let kept = made.marks.insert(mark)
                            // A picture the file held is listed with the artwork's pictures, so
                            // the engine is given its bytes by the address the mark names.
                            if kept.kind == "image"
                                and kept.src != ""
                                and made.pictures.find(p -> p.url == kept.src) == .none {
                                made.pictures.insert(Picture(name: kept.name, url: kept.src))
                            }
                        }
                    }
                }
                if let area = object_field(opened, "selection") {
                    made.selection = .some(
                        Area(
                            d: text_field(area, "d"),
                            feather: number_field(area, "feather") ?? 0.0,
                        ),
                    )
                }
                made
            }
            .err(why) => blank_with(lib, why),
        },
        .err(why) => blank_with(lib, why),
    }
}

/// The mark a file's `row` is, on `layer`: a stroke keeps its points, its brush and its ink, and
/// a shape its outline, wherever the engine can name that outline as a path this app draws.
fn mark_of(layer: Layer, row: json.Value) -> Option<Mark> {
    if let shape = object_field(row, "shape") {
        let kind = text_field(shape, "kind")
        let fill = nested_text(row, "style", "fill")
        let stroke = nested_text(row, "style", "stroke")
        let brush = nested_text(row, "style", "brush")
        let opacity = nested_number(row, "style", "opacity") ?? 1.0
        if kind == "stroke" {
            let points: List<List<Float>> = nested_points(row, "shape", "points")
            return .some(
                Mark(
                    name: text_field(row, "name"),
                    layer: layer,
                    kind: "stroke",
                    points: points.flat_map(x -> x),
                    size: number_field(shape, "size") ?? 6.0,
                    colour: stroke,
                    brush: brush,
                    opacity: opacity,
                ),
            )
        }
        if kind == "image" and text_field(shape, "src") != "" {
            return .some(
                Mark(
                    name: text_field(row, "name"),
                    layer: layer,
                    kind: "image",
                    src: text_field(shape, "src"),
                    x: number_field(shape, "x") ?? 0.0,
                    y: number_field(shape, "y") ?? 0.0,
                    width: number_field(shape, "width") ?? 0.0,
                    height: number_field(shape, "height") ?? 0.0,
                    opacity: opacity,
                ),
            )
        }
        if let d = path_of(shape) {
            return .some(Mark(layer: layer, kind: "path", d: d, colour: fill, opacity: opacity))
        }
    }
    .none
}

/// A blank artwork begun with a problem to show, so a file that cannot be opened still lands
/// where a person can read why.
action blank_with(lib: Library, why: Text) -> Artwork {
    let made = new_artwork(lib, "Untitled artwork", 1024.0, 768.0)
    made.notice = why
    made
}

/// Starts a blank artwork with one plain layer.
action new_artwork(lib: Library, title: Text, width: Float, height: Float) -> Artwork (
    icon: .file_plus,
    label: "New artwork",
    did: "started a new artwork",
) {
    require width >= 16.0 and width <= 8192.0 "An artwork is 16 to 8192 units wide"
    require height >= 16.0 and height <= 8192.0 "An artwork is 16 to 8192 units high"
    let made = lib.artworks.insert(Artwork(title: title, width: width, height: height))
    made.layers.insert(Layer(name: "Layer 1"))
    made
}

/// Adds a layer on top of the artwork.
action add_layer(a: Artwork, name: Text = "") -> Layer (
    icon: .plus,
    label: "Add a layer",
    did: "added a layer",
) {
    require name.trim() == "" or a.layers.find(l -> l.name == name.trim()) == .none "A layer named {name.trim()} is already here"
    let named = if name.trim() == "" { free_layer_name(a, "Layer") } else { name.trim() }
    a.layers.insert(Layer(name: named))
}

/// Renames a layer. Two layers never share a name: a mark names its layer by it.
action rename_layer(a: Artwork, layer: Layer, name: Text) (
    label: "Rename a layer",
    did: "renamed a layer",
) {
    require name.trim() != "" "A layer has a name"
    require a.layers.find(l -> l != layer and l.name == name.trim()) == .none "Another layer is named {name.trim()}"
    layer.name = name.trim()
}

/// Holds a layer still, so the pointer passes through its marks, or lets its marks be picked
/// again.
action lock_layer(a: Artwork, layer: Layer, locked: Bool = not layer.locked) (
    icon: .lock,
    label: "Lock a layer",
    did: "locked a layer",
) {
    layer.locked = locked
}

/// Moves a layer one place up in the stack.
action move_layer_up(a: Artwork, layer: Layer) (label: "Move a layer up", did: "moved a layer") {
    let at = a.layers.index_of(layer) ?? 0
    if at + 1 < a.layers.len() {
        if let above = a.layers.at(at + 1) { a.layers.move(layer, after: above) }
    }
}

/// Moves a layer one place down in the stack.
action move_layer_down(a: Artwork, layer: Layer) (
    label: "Move a layer down",
    did: "moved a layer",
) {
    let at = a.layers.index_of(layer) ?? 0
    if at > 0 {
        if let below = a.layers.at(at - 1) { a.layers.move(layer, before: below) }
    }
}

/// Copies a layer and everything on it, above the original.
action duplicate_layer(a: Artwork, layer: Layer) -> Layer (
    icon: .copy,
    label: "Duplicate a layer",
    did: "duplicated a layer",
) {
    let made = a.layers.insert(
        Layer(
            name: free_layer_name(a, "{layer.name} copy"),
            medium: layer.medium,
            opacity: layer.opacity,
            visible: layer.visible,
            locked: layer.locked,
            texture: layer.texture,
        ),
    )
    for m in a.marks.filter(m -> m.layer == layer) {
        a.marks.insert(
            Mark(
                name: m.name,
                layer: made,
                kind: m.kind,
                points: m.points,
                d: m.d,
                size: m.size,
                colour: m.colour,
                brush: m.brush,
                opacity: m.opacity,
                src: m.src,
                x: m.x,
                y: m.y,
                width: m.width,
                height: m.height,
            ),
        )
    }
    a.layers.move(made, after: layer)
    made
}

/// Folds a layer into the one under it: its marks join that layer, and it is gone.
action merge_layer_down(a: Artwork, layer: Layer) (
    label: "Merge a layer down",
    did: "merged a layer",
) {
    let at = a.layers.index_of(layer) ?? 0
    if at == 0 {
        a.notice = "The bottom layer has nothing under it to merge into"
        return
    }
    if let below = a.layers.at(at - 1) {
        for m in a.marks.filter(m -> m.layer == layer) {
            m.layer = below
        }
        a.layers.remove(layer)
    }
}

/// The name a new layer at `base` takes: the base itself, or the first `base 2`, `base 3` … the
/// artwork does not know, so every layer's name tells it from the others.
fn free_layer_name(a: Artwork, base: Text) -> Text {
    if a.layers.find(l -> l.name == base) == .none { base } else { free_layer_after(a, base, 2) }
}

fn free_layer_after(a: Artwork, base: Text, n: Int) -> Text {
    if a.layers.find(l -> l.name == "{base} {n}") == .none {
        "{base} {n}"
    } else { free_layer_after(a, base, n + 1) }
}

/// Shows or hides a layer.
action show_layer(a: Artwork, layer: Layer, visible: Bool) (
    label: "Show or hide a layer",
    did: "hid or showed a layer",
) {
    layer.visible = visible
}

destructive action remove_layer(a: Artwork, layer: Layer) (icon: .trash, label: "Remove a layer") {
    for m in a.marks.filter(m -> m.layer == layer) {
        a.marks.remove(m)
    }
    a.layers.remove(layer)
}

/// Sets what the artwork is called and how big it is.
action set_artwork(a: Artwork, title: Text = "", width: Float = 0.0, height: Float = 0.0) (
    label: "Set the artwork",
    did: "changed the artwork",
) {
    if title != "" {
        a.title = title
    }
    if width >= 16.0 and width <= 8192.0 {
        a.width = width
    }
    if height >= 16.0 and height <= 8192.0 {
        a.height = height
    }
}

/// Lays one brush stroke where the hand went, its point `[x, y, pressure]` in artwork units.
action add_stroke(
    a: Artwork,
    layer: Option<Layer>,
    point: List<Float>,
    size: Float,
    colour: Text,
    brush: Text,
) -> Mark (icon: .pencil, label: "Paint", did: "painted a stroke") {
    require point.len() >= 2 "A stroke point is x and y"
    require unlocked(layer) "The layer is locked"
    let on = if let chosen = layer { chosen } else { top_layer(a) }
    let made = a.marks.insert(
        Mark(
            layer: on,
            kind: "stroke",
            points: point,
            size: size.clamp(0.25, 512.0),
            colour: colour,
            brush: brush,
        ),
    )
    made.name = "painting"
    made.open = true
    made
}

/// Adds the next point to the stroke being painted.
action extend_stroke(a: Artwork, m: Mark, point: List<Float>) (
    label: "Paint",
    did: "painted a stroke",
) {
    m.points = [m.points, point].flat_map(x -> x)
}

/// Whether paint may land on `layer`: a layer the person chose and held still takes none, and
/// one not chosen hands the paint to [`top_layer`].
fn unlocked(layer: Option<Layer>) -> Bool {
    if let chosen = layer { not chosen.locked } else { true }
}

/// The layer a stroke lands on when none is chosen: the top one that shows and is not held
/// still. An artwork whose layers are all hidden or locked gains one to paint on.
action top_layer(a: Artwork) -> Layer {
    if let top = a.layers.filter(l -> l.visible and not l.locked).last() {
        top
    } else {
        a.layers.insert(Layer(name: free_layer_name(a, "Layer")))
    }
}

/// Draws a shape, its SVG path data in artwork units.
action add_shape(a: Artwork, layer: Option<Layer>, d: Text, colour: Text) -> Mark (
    icon: .square,
    label: "Draw a shape",
    did: "drew a shape",
) {
    require d.trim() != "" "A shape has an outline"
    require unlocked(layer) "The layer is locked"
    let on = if let chosen = layer { chosen } else { top_layer(a) }
    a.marks.insert(Mark(layer: on, kind: "path", d: d, colour: colour))
}

destructive action remove_mark(a: Artwork, m: Mark) (icon: .trash, label: "Remove a mark") {
    a.marks.remove(m)
}

/// Recolours a mark.
action recolour(a: Artwork, m: Mark, colour: Text) (
    label: "Change a mark's colour",
    did: "recoloured a mark",
) {
    m.colour = colour
}

/// Makes a mark bigger or smaller.
action resize_mark(a: Artwork, m: Mark, size: Float) (label: "Size a mark", did: "sized a mark") {
    require size > 0.0 "A mark's brush is wider than nothing"
    m.size = size
}

/// Places a picture the person chose on the top layer, fitted inside the artwork and centred:
/// the picture stays its own object, so it moves, crops and syncs like paint.
action place_picture(a: Artwork, file: File) -> Mark uses image (
    icon: .image,
    label: "Place a picture",
    did: "placed a picture",
) {
    require file.url != "" "A picture is placed from a file"
    let on = top_layer(a)
    if a.pictures.find(p -> p.url == file.url) == .none {
        a.pictures.insert(Picture(name: file.name, url: file.url))
    }
    let natural_width = if let info = file.image_info() { info.width.to_float() } else { a.width }
    let natural_height = if let info = file.image_info() {
        info.height.to_float()
    } else { a.height }
    let fit = (a.width / natural_width.max(1.0)).min(a.height / natural_height.max(1.0))
    let width = natural_width * fit
    let height = natural_height * fit
    a.marks.insert(
        Mark(
            name: file.name,
            layer: on,
            kind: "image",
            src: file.url,
            x: (a.width - width) / 2.0,
            y: (a.height - height) / 2.0,
            width: width,
            height: height,
        ),
    )
}

/// Selects an area, its outline as SVG path data.
action select_area(a: Artwork, d: Text, feather: Float = 0.0) (
    icon: .circle_dashed,
    label: "Select an area",
    did: "selected an area",
) {
    require d.trim() != "" "A selection has an outline"
    require feather >= 0.0 and feather <= 500.0 "A feather is 0 to 500 units"
    a.selection = .some(Area(d: d, feather: feather))
}

/// Lets go of the selection.
action clear_selection(a: Artwork) (label: "Deselect") {
    a.selection = .none
}

/// Fills the selection as a path mark on a layer.
action fill_selection(a: Artwork, layer: Option<Layer>, colour: Text) -> Option<Mark> (
    icon: .paint_bucket,
    label: "Fill the selection",
    did: "filled the selection",
) {
    if let area = a.selection {
        return .some(add_shape(a, layer, area.d, colour))
    }
    a.notice = "Nothing is selected; select part of the artwork first"
    return .none
}

/// The artwork as the device's drawing engine reads it: its fields and its marks', each mark
/// naming the layer it stands on by that layer's name. The engine reads this shape and makes no
/// store of its own, so what a person paints reaches it exactly as the records hold it.
fn artwork_json(a: Artwork) -> Text {
    json.encode(
        ArtworkJson(
            title: a.title,
            width: a.width,
            height: a.height,
            ground_texture: a.ground_texture,
            ground_colour: a.ground_colour,
            layers: a.layers.map(l -> layer_json(l)),
            marks: a.marks.map(m -> mark_json(m)),
            selection: area_json(a),
        ),
    )
}

fn layer_json(l: Layer) -> LayerJson {
    LayerJson(
        name: l.name,
        medium: l.medium,
        opacity: l.opacity,
        visible: l.visible,
        locked: l.locked,
        texture: l.texture,
    )
}

fn mark_json(m: Mark) -> MarkJson {
    MarkJson(
        name: m.name,
        layer: layer_json(m.layer),
        kind: m.kind,
        points: m.points,
        d: m.d,
        size: m.size,
        colour: m.colour,
        brush: m.brush,
        opacity: m.opacity,
        src: m.src,
        x: m.x,
        y: m.y,
        width: m.width,
        height: m.height,
    )
}

fn area_json(a: Artwork) -> Option<AreaJson> {
    if let area = a.selection {
        return .some(AreaJson(d: area.d, feather: area.feather))
    }
    .none
}

fn picture_addresses(a: Artwork) -> List<Text> {
    a.pictures.map(p -> p.url)
}

/// The picture of the artwork as it stands, kept on it, so the surface shows it under the hand.
action draw_look(a: Artwork, size: Int = 768) (icon: .eye, label: "Look at the artwork") {
    let request = json.encode(LookRequest(size: size, format: "png"))
    match canvas.look(artwork_json(a), request, picture_addresses(a)) {
        .ok(file) => {
            a.look = file.url
            a.notice = ""
        }
        .err(why) => {
            a.notice = why
        }
    }
}

/// Writes the artwork at full size as a PNG, JPEG or WebP picture.
action export_picture(a: Artwork, format: Text = "png", scale: Float = 1.0) -> Option<File> (
    icon: .download,
    label: "Export a picture",
    did: "exported a picture",
) {
    let request = json.encode(ExportRequest(format: format, scale: scale))
    match canvas.export(artwork_json(a), request, picture_addresses(a)) {
        .ok(file) => .some(file),
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// Writes the artwork as an SVG document.
action export_svg(a: Artwork) -> Text (
    icon: .download,
    label: "Export SVG",
    did: "exported an SVG",
) {
    match canvas.svg(artwork_json(a), json.encode(SvgRequest(ids: .none, outline_text: .none))) {
        .ok(markup) => {
            a.notice = ""
            markup
        }
        .err(why) => {
            a.notice = why
            ""
        }
    }
}

/// Saves the artwork as a `.artwork` package carrying its pictures.
action save_artwork(a: Artwork) -> Option<File> (
    icon: .save,
    label: "Save the artwork",
    did: "saved the artwork",
) {
    match canvas.save(artwork_json(a), picture_addresses(a)) {
        .ok(file) => .some(file),
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// Imports an SVG document as marks on a layer: each shape the device's drawing engine read from
/// it becomes a mark, so the import is drawn, saved and shown like paint the person laid.
action import_svg(a: Artwork, markup: Text) -> Text (
    icon: .file_plus,
    label: "Import an SVG",
    did: "imported an SVG",
) {
    match canvas.svg_import(
        markup,
        json.encode(PlaceRequest(layer: .none, group: .none, fit: .none)),
    ) {
        .ok(drawn) => match json.parse(drawn) {
            .ok(import) => {
                let on = top_layer(a)
                let before = a.marks.len()
                for row in array_field(import, "marks") {
                    if let mark = drawn_mark(on, row) { a.marks.insert(mark) }
                }
                a.notice = "Imported {a.marks.len() - before} shapes from the SVG"
                drawn
            }
            .err(why) => {
                a.notice = why
                ""
            }
        },
        .err(why) => {
            a.notice = why
            ""
        }
    }
}

/// An imported mark on `layer`, made of what the SVG said: a path for the shapes a path draws,
/// and a stroke where the SVG holds brush strokes.
fn drawn_mark(layer: Layer, row: json.Value) -> Option<Mark> {
    if let shape = object_field(row, "shape") {
        let kind = text_field(shape, "kind")
        let fill = text_field(row, "fill")
        let colour = if fill != "" { fill } else { text_field(row, "stroke") }
        let opacity = number_field(row, "opacity") ?? 1.0
        if kind == "stroke" {
            let points = points_field(shape, "points")
            return .some(
                Mark(
                    name: text_field(row, "name"),
                    layer: layer,
                    kind: "stroke",
                    points: points.flat_map(x -> x),
                    size: number_field(shape, "size") ?? 6.0,
                    colour: colour,
                    brush: text_field(row, "brush"),
                    opacity: opacity,
                ),
            )
        }
        if let d = path_of(shape) {
            return .some(Mark(layer: layer, kind: "path", d: d, colour: colour, opacity: opacity))
        }
    }
    .none
}

/// The outline a shape's geometry stands for, as SVG path data; nothing for text or a picture,
/// which this app's marks have no form of their own for.
fn path_of(shape: json.Value) -> Option<Text> {
    let kind = text_field(shape, "kind")
    let points = points_field(shape, "points")
    if kind == "path" and text_field(shape, "d") != "" {
        return .some(text_field(shape, "d"))
    }
    if kind == "rect" {
        let x = number_field(shape, "x") ?? 0.0
        let y = number_field(shape, "y") ?? 0.0
        let width = number_field(shape, "width") ?? 0.0
        let height = number_field(shape, "height") ?? 0.0
        return .some(rect_path([x, y], [x + width, y + height]))
    }
    if kind == "ellipse" {
        let cx = number_field(shape, "cx") ?? 0.0
        let cy = number_field(shape, "cy") ?? 0.0
        let rx = number_field(shape, "rx") ?? 0.0
        let ry = number_field(shape, "ry") ?? 0.0
        return .some(
            "M {cx - rx} {cy} A {rx} {ry} 0 1 0 {cx + rx} {cy} A {rx} {ry} 0 1 0 {cx - rx} {cy} Z",
        )
    }
    if kind == "polygon" and points.len() >= 3 {
        return .some(closed_path(points))
    }
    if kind == "line" and points.len() == 2 {
        if let from = points.at(0), to = points.at(1) {
            return .some(line_path(from, to, number_field(shape, "size") ?? 6.0))
        }
    }
    .none
}

/// The point lists at `key` of a JSON object: each point an array of numbers.
fn points_field(value: json.Value, key: Text) -> List<List<Float>> {
    if let held = object_field(value, key), items = as_array(held) {
        items.map(item -> numbers_of(item))
    } else { [] }
}

fn nested_points(value: json.Value, object: Text, key: Text) -> List<List<Float>> {
    if let held = object_field(value, object) { points_field(held, key) } else { [] }
}

fn numbers_of(value: json.Value) -> List<Float> {
    if let items = as_array(value) {
        items.map(item -> as_number(item) ?? 0.0)
    } else { [] }
}

/// The value at `key` of a JSON object, when it holds one there.
fn object_field(value: json.Value, key: Text) -> Option<json.Value> {
    match as_object(value) {
        .some(fields) => fields.get(key),
        .none => .none,
    }
}

/// The value as a JSON object's fields, when it is an object.
fn as_object(value: json.Value) -> Option<Map<json.Value>> {
    match value {
        .object(fields) => .some(fields),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The value as a JSON array's items, when it is an array.
fn as_array(value: json.Value) -> Option<List<json.Value>> {
    match value {
        .object(fields) => .none,
        .array(items) => .some(items),
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The value as text, when it is a JSON string.
fn as_text(value: json.Value) -> Option<Text> {
    match value {
        .object(fields) => .none,
        .array(items) => .none,
        .string(text) => .some(text),
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The value as a flag, when it is a JSON boolean.
fn as_bool(value: json.Value) -> Option<Bool> {
    match value {
        .object(fields) => .none,
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .some(flag),
        .null => .none,
    }
}

/// The array at `key` of a JSON object, and nothing when it holds none there.
fn array_field(value: json.Value, key: Text) -> List<json.Value> {
    match object_field(value, key) {
        .some(held) => as_array(held) ?? [],
        .none => [],
    }
}

/// The text at `key` of a JSON object, and nothing when it holds none there.
fn text_field(value: json.Value, key: Text) -> Text {
    match object_field(value, key) {
        .some(held) => as_text(held) ?? "",
        .none => "",
    }
}

/// The value as a JSON number, when it is one.
fn as_number(value: json.Value) -> Option<Float> {
    match value {
        .object(fields) => .none,
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .some(number),
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The number at `key` of a JSON object.
fn number_field(value: json.Value, key: Text) -> Option<Float> {
    match object_field(value, key) {
        .some(held) => as_number(held),
        .none => .none,
    }
}

/// The flag at `key` of a JSON object.
fn flag_field(value: json.Value, key: Text) -> Option<Bool> {
    match object_field(value, key) {
        .some(held) => as_bool(held),
        .none => .none,
    }
}

/// The text at `key` of the object `object` holds at `of`.
fn nested_text(value: json.Value, of: Text, key: Text) -> Text {
    if let held = object_field(value, of) { text_field(held, key) } else { "" }
}

fn nested_number(value: json.Value, of: Text, key: Text) -> Option<Float> {
    if let held = object_field(value, of) { number_field(held, key) } else { .none }
}

/// The closed outline through `points`.
fn closed_path(points: List<List<Float>>) -> Text {
    let corners = points.map(p -> "{p.at(0) ?? 0.0} {p.at(1) ?? 0.0}")
    "M {corners.join(" L ")} Z"
}

/// Clears the notice a person has read.
action clear_notice(a: Artwork) {
    a.notice = ""
}

// ---- the window ----
view artwork(a: Artwork) {
    state tool: Text = "brush"
    state brush: Text = "round"
    state size: Float = 6.0
    state colour: Text = "#1d1b19"
    state chosen: Option<Layer> = .none
    state show_layers: Bool = true
    // Where the drag a shape is drawn with began, as `[x, y]`: the new stroke's start while a
    // brush is out, and the first corner of the shape while a shape tool is.
    state shape_from: List<Float> = []
    state zoom: Float = 1.0
    state pan_x: Float = 0.0
    state pan_y: Float = 0.0
    std.page(
        a,
        canvas: true,
        fill: true,
        header: {
            artwork_head(a, tool, brush, size, colour, show_layers, zoom, pan_x, pan_y)
        },
    ) {
        if a.notice != "" {
            ui.Banner(a.notice, kind: .warning) {
                on act: clear_notice(a)
            }
        }
        row(gap: .none, fill) {
            if show_layers {
                layers_rail(a, chosen)
            }
            stack(gap: .none, fill) {
                ui.PaintSurface(
                    picture: a.look,
                    width: a.width,
                    height: a.height,
                    brush: paint_brush(tool, brush),
                    size: size,
                    colour: colour,
                    zoom: zoom,
                    pan: [pan_x, pan_y],
                    label: a.title,
                ) {
                    on stroke_start(p: List<Float>): set(shape_from, p)
                        .then(
                            start_paint(a, tool, paint_brush(tool, brush), chosen, p, size, colour),
                        )
                    on stroke(p: List<Float>): continue_paint(
                        a,
                        tool,
                        paint_brush(tool, brush),
                        p,
                        size,
                        colour,
                    )
                    on stroke_end(p: List<Float>): finish_paint(
                        a,
                        tool,
                        brush,
                        chosen,
                        shape_from,
                        p,
                        size,
                        colour,
                    )
                    // The wheel and a pinch zoom about the view, and a middle-button drag pans it.
                    on wheel(delta): set(zoom, zoomed(zoom, delta))
                    on pinch(scale): set(zoom, (zoom * scale).clamp(0.05, 64.0))
                    on panned(delta): set(pan_x, pan_x + (delta.at(0) ?? 0.0))
                        .then(set(pan_y, pan_y + (delta.at(1) ?? 0.0)))
                }
                well(a, tool, brush, size, colour, pan_x, pan_y, zoom)
            }
        }
    }
}

/// The window's own controls: the tools, the view, and what makes and keeps the artwork.
view artwork_head(
    a: Artwork,
    tool: State<Text>,
    brush: State<Text>,
    size: Float,
    colour: Text,
    show_layers: State<Bool>,
    zoom: State<Float>,
    pan_x: State<Float>,
    pan_y: State<Float>,
) {
    ui.Toolbar {
        ui.ToolbarButton("Brush", icon: .pencil, toggled: tool == "brush") {
            on press: set(tool, "brush")
        }
        ui.ToolbarButton("Eraser", icon: .eraser, toggled: tool == "eraser") {
            on press: set(tool, "eraser")
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Rectangle", icon: .square, toggled: tool == "rect") {
            on press: set(tool, "rect")
        }
        ui.ToolbarButton("Ellipse", icon: .circle, toggled: tool == "ellipse") {
            on press: set(tool, "ellipse")
        }
        ui.ToolbarButton("Line", icon: .minus, toggled: tool == "line") {
            on press: set(tool, "line")
        }
        ui.ToolbarButton("Select", icon: .circle_dashed, toggled: tool == "select") {
            on press: set(tool, "select")
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Zoom out", icon: .zoom_out, joined: true) {
            on press: set(zoom, (zoom / 1.25).max(0.05))
        }
        ui.ToolbarButton("Zoom in", icon: .zoom_in, joined: true) {
            on press: set(zoom, (zoom * 1.25).min(64.0))
        }
        ui.ToolbarButton("Fit", icon: .maximize) {
            on press: set(zoom, 1.0).then(set(pan_x, 0.0)).then(set(pan_y, 0.0))
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("New artwork", icon: .plus) {
            on press: new_artwork(root Library, "Untitled artwork", 1024.0, 768.0)
        }
        ui.ToolbarButton("Add layer", icon: .layers) {
            on press: add_layer(a, "")
        }
        ui.ToolbarButton("Place a picture", icon: .image_plus) {
            on press: file.choose(
                place_picture(a),
                accept: ["image/png", "image/jpeg", "image/webp"],
            )
        }
        ui.ToolbarButton("Look", icon: .eye) {
            on press: draw_look(a)
        }
        ui.ToolbarButton("Save", icon: .save) {
            on press: save_artwork(a)
        }
        ui.ToolbarSpacer()
        ui.ToolbarButton("Layers", icon: .panel_left, toggled: show_layers) {
            on press: set(show_layers, not show_layers)
        }
    }
}

/// The layers, each with what makes it what it is: shown or hidden, locked, its opacity and how
/// much of the ground shows through it, moved up or down the stack, duplicated, merged, removed.
view layers_rail(a: Artwork, chosen: State<Option<Layer>>) {
    stack(gap: .related, fill) {
        ui.Text("Layers", role: .heading)
        stack(gap: .group) {
            for layer in a.layers {
                stack(gap: .tight) {
                    row(gap: .tight, align: .center) {
                        ui.Button(
                            if layer.visible { "Hide the layer" } else { "Show the layer" },
                            icon: if layer.visible { .eye } else { .eye_off },
                            icon_only: true,
                        ) {
                            on press: show_layer(a, layer, not layer.visible)
                        }
                        ui.ListRow(
                            title: layer.name,
                            selected: chosen == .some(layer),
                            icon: if layer.locked { .lock } else { .image },
                            note: layer.medium,
                        ) {
                            on activate: set(chosen, .some(layer))
                        }
                        ui.MenuButton(
                            "Layer actions",
                            icon: .ellipsis,
                            icon_only: true,
                            target: layer,
                        )
                    }
                    row(gap: .tight, align: .center) {
                        ui.Text("Opacity", role: .caption, secondary: true)
                        ui.Slider(bind: layer.opacity, label: "Opacity", min: 0.0, max: 1.0)
                        ui.Button("Up", icon: .chevron_up, icon_only: true) {
                            on press: move_layer_up(a, layer)
                        }
                        ui.Button("Down", icon: .chevron_down, icon_only: true) {
                            on press: move_layer_down(a, layer)
                        }
                    }
                }
            }
        }
        ui.Button("Add layer", icon: .plus, role: .plain, size: .small) {
            on press: add_layer(a, "")
        }
    }
}

/// The paint loaded: the brush, its size and its colour, and what the artwork leaves as.
view well(
    a: Artwork,
    tool: State<Text>,
    brush: State<Text>,
    size: State<Float>,
    colour: State<Text>,
    pan_x: State<Float>,
    pan_y: State<Float>,
    zoom: State<Float>,
) {
    row(gap: .related, align: .center, wrap: true) {
        ui.MenuButton("Brush: {brush}", icon: .pencil) {
            for preset in brushes() {
                ui.MenuItem(preset) {
                    on choose: set(brush, preset)
                }
            }
        }
        ui.Text(paint_brush(tool, brush), role: .caption, secondary: true)
        ui.Slider(bind: size, label: "Size", min: 1.0, max: 128.0, fill)
        ui.ColorPicker(bind: colour, swatches: inks(), columns: 7, label: "Paint")
        if tool == "select" {
            ui.Button("Fill the selection", icon: .paint_bucket) {
                on press: fill_selection(a, .none, colour)
            }
            ui.Button("Deselect", icon: .circle_dashed) {
                on press: clear_selection(a)
            }
        }
        ui.MenuButton("Export", icon: .download) {
            ui.MenuItem("PNG, full size") {
                on choose: file.save(export_picture(a, "png", 1.0), name: "Artwork.png")
            }
            ui.MenuItem("PNG, twice the size") {
                on choose: file.save(export_picture(a, "png", 2.0), name: "Artwork.png")
            }
            ui.MenuItem("JPEG, full size") {
                on choose: file.save(export_picture(a, "jpeg", 1.0), name: "Artwork.jpg")
            }
            ui.MenuItem("WebP, full size") {
                on choose: file.save(export_picture(a, "webp", 1.0), name: "Artwork.webp")
            }
            ui.MenuItem("SVG") {
                on choose: file.save(export_svg(a), name: "Artwork.svg")
            }
        }
        ui.Text("{zoomed_said(zoom)}", role: .caption, secondary: true)
        ui.Button("Fit the view", icon: .maximize, icon_only: true) {
            on press: set(zoom, 1.0).then(set(pan_x, 0.0)).then(set(pan_y, 0.0))
        }
    }
}

fn zoomed_said(zoom: Float) -> Text {
    "{(zoom * 100.0).round()}%"
}

/// The brushes the pad lays: the engine's presets, by the names a mark keeps.
fn brushes() -> List<Text> {
    [
        "round",
        "pen",
        "pencil",
        "marker",
        "airbrush",
        "charcoal",
        "calligraphy",
        "eraser",
        "pixel",
        "pixel-eraser",
    ]
}

/// The inks a person reaches for, without opening a colour wheel.
fn inks() -> List<Text> {
    [
        "#1d1b19",
        "#ffffff",
        "#d24b3a",
        "#e39a2f",
        "#e2c33a",
        "#3f9a52",
        "#2f7fb8",
        "#2f4fa8",
        "#7a4fb8",
        "#c2479a",
        "#8a5a32",
        "#6b6b6b",
    ]
}

/// The brush a tool paints with: the chosen preset, or the eraser when the eraser is out.
fn paint_brush(tool: Text, brush: Text) -> Text {
    if tool == "eraser" { "eraser" } else { brush }
}

fn paints_a_stroke(tool: Text) -> Bool {
    tool == "brush" or tool == "eraser"
}

/// Starts what the hand lays down at `point`: a stroke with the paint loaded, or nothing when the
/// tool draws a shape, whose outline only exists once the hand lifts. It lands on `layer`, the
/// one the person chose, or the top one that takes paint.
action start_paint(
    a: Artwork,
    tool: Text,
    brush: Text,
    layer: Option<Layer>,
    point: List<Float>,
    size: Float,
    colour: Text,
) {
    if not paints_a_stroke(tool) {
        return
    }
    // A stroke whose end never came is finished by the next one.
    for m in a.marks.filter(m -> m.open) {
        m.open = false
    }
    add_stroke(a, layer, point, size, colour, brush)
}

/// Takes the next point of the stroke being painted. A shape tool paints nothing while the hand
/// moves: its outline is drawn when the hand lifts.
action continue_paint(
    a: Artwork,
    tool: Text,
    brush: Text,
    point: List<Float>,
    size: Float,
    colour: Text,
) -> Option<Mark> {
    if not paints_a_stroke(tool) {
        return .none
    }
    if let painting = a.marks.filter(m -> m.open).last() {
        extend_stroke(a, painting, point)
        return .some(painting)
    }
    .some(add_stroke(a, .none, point, size, colour, brush))
}

/// Ends what the hand laid down at `point`: the stroke takes its last point and closes, or a
/// shape is drawn across the drag that made it.
action finish_paint(
    a: Artwork,
    tool: Text,
    brush: Text,
    layer: Option<Layer>,
    from: List<Float>,
    point: List<Float>,
    size: Float,
    colour: Text,
) -> Option<Mark> {
    if paints_a_stroke(tool) {
        if let made = continue_paint(a, tool, brush, point, size, colour) {
            made.open = false
            return .some(made)
        }
        let made = add_stroke(a, .none, point, size, colour, brush)
        made.open = false
        return .some(made)
    }
    if from.len() < 2 {
        return .none
    }
    if tool == "rect" and not same_place(from, point) {
        return .some(add_shape(a, layer, rect_path(from, point), colour))
    }
    if tool == "ellipse" and not same_place(from, point) {
        return .some(add_shape(a, layer, ellipse_path(from, point), colour))
    }
    if tool == "line" and not same_place(from, point) {
        return .some(add_shape(a, layer, line_path(from, point, size), colour))
    }
    if tool == "select" and not same_place(from, point) { select_area(a, rect_path(from, point)) }
    .none
}

fn same_place(from: List<Float>, to: List<Float>) -> Bool {
    (from.at(0) ?? 0.0) == (to.at(0) ?? 1.0) and (from.at(1) ?? 0.0) == (to.at(1) ?? 1.0)
}

/// How much the wheel turns the view: `[across, down]` in logical pixels, where down zooms out
/// and up zooms in.
fn zoomed(zoom: Float, delta: List<Float>) -> Float {
    (zoom * (1.0 - (delta.at(1) ?? 0.0) / 500.0)).clamp(0.05, 64.0)
}

/// The outline of a rectangle from one corner of the drag to the other.
fn rect_path(from: List<Float>, to: List<Float>) -> Text {
    let x0 = from.at(0) ?? 0.0
    let y0 = from.at(1) ?? 0.0
    let x1 = to.at(0) ?? 0.0
    let y1 = to.at(1) ?? 0.0
    "M {x0} {y0} L {x1} {y0} L {x1} {y1} L {x0} {y1} Z"
}

/// The outline of an ellipse inside the rectangle the drag made.
fn ellipse_path(from: List<Float>, to: List<Float>) -> Text {
    let x0 = from.at(0) ?? 0.0
    let y0 = from.at(1) ?? 0.0
    let x1 = to.at(0) ?? 0.0
    let y1 = to.at(1) ?? 0.0
    let cx = (x0 + x1) / 2.0
    let cy = (y0 + y1) / 2.0
    let rx = (x1 - x0).abs() / 2.0
    let ry = (y1 - y0).abs() / 2.0
    "M {cx - rx} {cy} A {rx} {ry} 0 1 0 {cx + rx} {cy} A {rx} {ry} 0 1 0 {cx - rx} {cy} Z"
}

/// The outline of a line from one end of the drag to the other, as wide as the brush.
fn line_path(from: List<Float>, to: List<Float>, size: Float) -> Text {
    let x0 = from.at(0) ?? 0.0
    let y0 = from.at(1) ?? 0.0
    let x1 = to.at(0) ?? 0.0
    let y1 = to.at(1) ?? 0.0
    let half = size.max(0.25) / 2.0
    let length = ((x1 - x0) * (x1 - x0) + (y1 - y0) * (y1 - y0)).sqrt().max(0.0001)
    let nx = (0.0 - (y1 - y0)) / length * half
    let ny = (x1 - x0) / length * half
    "M {x0 + nx} {y0 + ny} L {x1 + nx} {y1 + ny} L {x1 - nx} {y1 - ny} L {x0 - nx} {y0 - ny} Z"
}
