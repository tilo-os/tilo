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

root library: Library

default view library for Library

view library(lib: Library) {
    if lib.artworks.is_empty() {
        ui.EmptyState("No artwork yet", message: "New artwork starts one", icon: .image)
    } else {
        stack {
            for art in lib.artworks {
                ui.ListRow(title: art.title, icon: .image)
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
        .ok(text) => match json.decode<OpenedArtwork>(text) {
            .ok(opened) => {
                let made = lib.artworks.insert(
                    Artwork(title: opened.title, width: opened.width, height: opened.height),
                )
                for row in opened.layers {
                    made.layers.insert(
                        Layer(
                            name: row.name,
                            medium: row.medium,
                            opacity: row.opacity,
                            visible: row.visible,
                        ),
                    )
                }
                if made.layers.is_empty() {
                    made.layers.insert(Layer(name: "Layer 1"))
                }
                made
            }
            .err(why) => blank_with(lib, why),
        },
        .err(why) => blank_with(lib, why),
    }
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
    let named = if name == "" { "Layer {a.layers.len() + 1}" } else { name }
    a.layers.insert(Layer(name: named))
}

/// Renames a layer.
action rename_layer(a: Artwork, layer: Layer, name: Text) (
    label: "Rename a layer",
    did: "renamed a layer",
) {
    require name.trim() != "" "A layer has a name"
    layer.name = name
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
    made
}

/// Adds the next point to the stroke being painted.
action extend_stroke(a: Artwork, m: Mark, point: List<Float>) (
    label: "Paint",
    did: "painted a stroke",
) {
    m.points = [m.points, point].flat_map(x -> x)
}

/// The layer a stroke lands on when none is chosen: the top one that shows.
action top_layer(a: Artwork) -> Layer {
    if let top = a.layers.filter(l -> l.visible).last() {
        top
    } else { a.layers.insert(Layer(name: "Layer 1")) }
}

/// Draws a shape, its SVG path data in artwork units.
action add_shape(a: Artwork, layer: Option<Layer>, d: Text, colour: Text) -> Mark (
    icon: .square,
    label: "Draw a shape",
    did: "drew a shape",
) {
    require d.trim() != "" "A shape has an outline"
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

/// The artwork as the device's drawing engine reads it, and the addresses of its pictures.
fn artwork_json(a: Artwork) -> Text {
    json.encode(a)
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

/// Imports an SVG document as marks on a layer.
action import_svg(a: Artwork, markup: Text) -> Text (
    icon: .file_plus,
    label: "Import an SVG",
    did: "imported an SVG",
) {
    match canvas.svg_import(
        markup,
        json.encode(PlaceRequest(layer: .none, group: .none, fit: .none)),
    ) {
        .ok(drawn) => {
            a.notice = "Imported the SVG's shapes"
            drawn
        }
        .err(why) => {
            a.notice = why
            ""
        }
    }
}

/// Clears the notice a person has read.
action clear_notice(a: Artwork) {
    a.notice = ""
}

/// An artwork a `.artwork` file opened into: the fields this app keeps.
type OpenedArtwork {
    title: Text = ""
    width: Float = 1024.0
    height: Float = 768.0
    layers: List<OpenedLayer>
}

type OpenedLayer {
    name: Text = ""
    medium: Text = ""
    opacity: Float = 1.0
    visible: Bool = true
}

// ---- the window ----
view artwork(a: Artwork) {
    state brush: Text = "round"
    state size: Float = 6.0
    state colour: Text = "#1d1b19"
    state chosen: Option<Layer> = .none
    state show_layers: Bool = true
    std.page(
        a,
        canvas: true,
        fill: true,
        header: {
            artwork_head(a, brush, size, colour, show_layers)
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
                    brush: brush,
                    size: size,
                    colour: colour,
                    zoom: 1.0,
                    pan: [0.0, 0.0],
                    label: a.title,
                ) {
                    on stroke_start(p: List<Float>): paint_point(a, p, size, colour, brush)
                    on stroke(p: List<Float>): paint_point(a, p, size, colour, brush)
                    on stroke_end(p: List<Float>): paint_point(a, p, size, colour, brush)
                }
                well(a, brush, size, colour)
            }
        }
    }
}

/// The window's own controls.
view artwork_head(a: Artwork, brush: Text, size: Float, colour: Text, show_layers: State<Bool>) {
    ui.Toolbar {
        ui.ToolbarButton("Add layer", icon: .plus) {
            on press: add_layer(a, "")
        }
        ui.ToolbarSeparator()
        ui.ToolbarButton("Look", icon: .eye) {
            on press: draw_look(a)
        }
        ui.ToolbarButton("Export", icon: .download) {
            on press: export_picture(a, "png", 1.0)
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

/// The layers, and the brush and colour the window paints with.
view layers_rail(a: Artwork, chosen: State<Option<Layer>>) {
    stack(gap: .related, fill) {
        ui.Text("Layers", role: .heading)
        stack {
            for layer in a.layers {
                ui.ListRow(
                    title: layer.name,
                    selected: chosen == .some(layer),
                    icon: .image,
                    note: layer.medium,
                ) {
                    on activate: set(chosen, .some(layer))
                }
            }
        }
        ui.Button("Add layer", icon: .plus, role: .plain, size: .small) {
            on press: add_layer(a, "")
        }
    }
}

/// The paint loaded: the brush, its size and its colour.
view well(a: Artwork, brush: Text, size: State<Float>, colour: State<Text>) {
    row(gap: .related, align: .center) {
        ui.Text(brush, role: .caption, secondary: true)
        ui.Slider(bind: size, label: "Size", min: 1.0, max: 128.0, fill)
        ui.ColorPicker(bind: colour, swatches: inks(), columns: 7, label: "Paint")
    }
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

/// Paints the point the hand is at: the stroke being painted takes the next point, else a
/// stroke starts here.
action paint_point(a: Artwork, point: List<Float>, size: Float, colour: Text, brush: Text) -> Mark {
    if let painting = a.marks.filter(m -> m.name == "painting").last() {
        extend_stroke(a, painting, point)
        painting
    } else {
        add_stroke(a, .none, point, size, colour, brush)
    }
}
