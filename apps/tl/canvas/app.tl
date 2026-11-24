app so.tilo.canvas "Canvas" (
    icon: .image,
    category: .graphics,
    tagline: "Draw and paint that stays editable",
    keywords: ["draw", "drawing", "paint", "sketch", "whiteboard", "art", "illustration", "canvas", "vector", "watercolour", "oil", "zeichnen", "malen", "skizze", "zeichenbrett", "leinwand", "tafel"],
    document: library,
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
    /// The media presets saved in the artwork, which its layers may name.
    media: Collection<Medium>
    /// Pictures placed in the artwork, by the address their bytes are kept at.
    pictures: Collection<Picture>
    /// The area selected for the next edit, kept with the artwork.
    selection: Option<Area>
    /// The picture to work from, which `compare_reference` shows the artwork against; it is not
    /// part of the work and never exported.
    reference: Option<Reference> = .none
    /// The picture of the artwork as last drawn, so the surface shows paint under the hand.
    look: Text = ""
    /// The artwork beside, over or against its reference as last compared, kept to look at.
    comparison: Text = ""
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
    /// The folder the layer stands in; empty for an ungrouped layer. It changes nothing about
    /// stacking or compositing.
    group: Text = ""
    /// The name of the layer this one masks: its marks are drawn in greys, light showing the
    /// layer and dark hiding it. Empty for an ordinary layer.
    mask_of: Text = ""
    /// A mask's grey before any mark, 0 to 1.
    base: Float = 1.0
    invert: Bool = false
    /// Clipped to the layer below: it shows only where that layer has paint.
    clip: Bool = false
    /// A mask that stays put when its layer's marks move.
    unlinked: Bool = false
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
    /// The folder the mark stands in; empty for ungrouped. Organisational, like a layer's.
    group: Text = ""
    /// On a painted layer: paint into what is still wet instead of over dry paint.
    wet: Bool = false
    /// On a painted layer: the medium's parameters overridden for this mark.
    params: Map<Float>
    /// A picture's bytes, by the address they are kept at, and the frame it is drawn in.
    src: Text = ""
    x: Float = 0.0
    y: Float = 0.0
    width: Float = 0.0
    height: Float = 0.0
    /// How the mark mixes with what is under it: empty or `normal`, `multiply`, `screen`,
    /// `overlay`, `darken`, `lighten`, `color-dodge`, `color-burn`, `hard-light`, `soft-light`,
    /// `difference` or `exclusion`.
    blend: Text = ""
    /// The picture's frame inside its own pixels, as fractions of them: all of it by default.
    crop_x: Float = 0.0
    crop_y: Float = 0.0
    crop_width: Float = 1.0
    crop_height: Float = 1.0
    /// A turn inside the picture's frame, in degrees.
    straighten: Float = 0.0
    flip_h: Bool = false
    flip_v: Bool = false
    /// Colour and tone adjustments, applied when the artwork is shown and exported; zero (or
    /// off, for the flags) leaves a step out.
    exposure: Float = 0.0
    contrast: Float = 0.0
    highlights: Float = 0.0
    shadows: Float = 0.0
    saturation: Float = 0.0
    vibrance: Float = 0.0
    hue: Float = 0.0
    temperature: Float = 0.0
    tint: Float = 0.0
    grayscale: Bool = false
    invert: Bool = false
    blur: Float = 0.0
    sharpen: Float = 0.0
    vignette: Float = 0.0
    /// A gradient fill drawn instead of the plain colour: `linear` or `radial`, its two ends and
    /// its stops. Empty for a plain fill.
    gradient: Text = ""
    gradient_x0: Float = 0.0
    gradient_y0: Float = 0.0
    gradient_x1: Float = 0.0
    gradient_y1: Float = 0.0
    gradient_offsets: Option<List<Float>> = .none
    gradient_colours: Option<List<Text>> = .none
    /// A text mark's words, its type and its setting; `kind` is `text` for one.
    text: Text = ""
    font: Text = ""
    weight: Int = 0
    italic: Bool = false
    align: Text = ""
    line_height: Float = 0.0
    tracking: Float = 0.0
    text_path: Text = ""
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

/// A medium saved in the artwork: a preset of oil or watercolour the layers may be painted
/// with.
type Medium (title: label) {
    key: Text
    label: Text
    /// `oil` or `watercolour`.
    engine: Text = "oil"
    description: Text = ""
    /// The ground it suits; empty for any.
    ground: Text = ""
    params: Map<Float>
    /// The pigments it carries, loaded as `pigment:<name>`.
    palette: List<Pigment>
}

/// A pigment a medium carries.
type Pigment {
    /// Lowercase words joined by `-`, such as `burnt-sienna`.
    name: Text
    /// Masstone colour, `#rrggbb`.
    colour: Text
    /// Watercolour: how much it settles into the paper's tooth, 0 to 1.
    granulation: Float = 0.0
    /// How strongly it tints, around 1.
    strength: Float = 1.0
}

/// A picture to work from, kept by the address of its bytes: the artwork is compared against it.
type Reference {
    image: Text
    x: Float = 0.0
    y: Float = 0.0
    width: Float = 0.0
    height: Float = 0.0
    opacity: Float = 1.0
    visible: Bool = true
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
    media: List<MediumJson>
    selection: Option<AreaJson>
    reference: Option<ReferenceJson>
}

type LayerJson {
    name: Text
    medium: Text
    opacity: Float
    visible: Bool
    locked: Bool
    texture: Float
    group: Text
    mask_of: Text
    base: Float
    invert: Bool
    clip: Bool
    unlinked: Bool
}

type MediumJson {
    id: Text
    label: Text
    engine: Text
    description: Text
    ground: Text
    params: Map<Float>
    palette: List<PigmentJson>
}

type PigmentJson {
    name: Text
    colour: Text
    granulation: Float
    strength: Float
}

type ReferenceJson {
    image: Text
    frame: Option<List<Float>>
    opacity: Float
    visible: Bool
}

type MarkJson {
    name: Text
    layer: LayerJson
    kind: Text
    group: Text
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
    blend: Text
    crop_x: Float
    crop_y: Float
    crop_width: Float
    crop_height: Float
    straighten: Float
    flip_h: Bool
    flip_v: Bool
    exposure: Float
    contrast: Float
    highlights: Float
    shadows: Float
    saturation: Float
    vibrance: Float
    hue: Float
    temperature: Float
    tint: Float
    grayscale: Bool
    invert: Bool
    blur: Float
    sharpen: Float
    vignette: Float
    gradient_kind: Text
    gradient_x0: Float
    gradient_y0: Float
    gradient_x1: Float
    gradient_y1: Float
    gradient_offsets: List<Float>
    gradient_colours: List<Text>
    text: Text
    font: Text
    weight: Int
    italic: Bool
    align: Text
    line_height: Float
    tracking: Float
    text_path: Text
    wet: Bool
    params: Map<Float>
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
                            group: text_field(row, "group"),
                            mask_of: text_field(row, "mask_of"),
                            base: number_field(row, "base") ?? 1.0,
                            invert: flag_field(row, "invert") ?? false,
                            clip: flag_field(row, "clip") ?? false,
                            unlinked: flag_field(row, "unlinked") ?? false,
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
                            // The adjustments and the blend the file kept, on every kind of mark.
                            kept.group = text_field(row, "group")
                            kept.blend = nested_text(row, "style", "blend")
                            kept.exposure = nested_number(row, "style", "exposure") ?? 0.0
                            kept.contrast = nested_number(row, "style", "contrast") ?? 0.0
                            kept.highlights = nested_number(row, "style", "highlights") ?? 0.0
                            kept.shadows = nested_number(row, "style", "shadows") ?? 0.0
                            kept.saturation = nested_number(row, "style", "saturation") ?? 0.0
                            kept.vibrance = nested_number(row, "style", "vibrance") ?? 0.0
                            kept.hue = nested_number(row, "style", "hue") ?? 0.0
                            kept.temperature = nested_number(row, "style", "temperature") ?? 0.0
                            kept.tint = nested_number(row, "style", "tint") ?? 0.0
                            kept.grayscale = nested_flag(row, "style", "grayscale") ?? false
                            kept.invert = nested_flag(row, "style", "invert") ?? false
                            kept.blur = nested_number(row, "style", "blur") ?? 0.0
                            kept.sharpen = nested_number(row, "style", "sharpen") ?? 0.0
                            kept.vignette = nested_number(row, "style", "vignette") ?? 0.0
                            kept.flip_h = axis_negative(row, 0)
                            kept.flip_v = axis_negative(row, 3)
                            kept.wet = nested_flag(row, "style", "wet") ?? false
                            let params = nested_params(row, "style", "params")
                            for key in params.keys() {
                                kept.params.put(key, params.get(key) ?? 0.0)
                            }
                            if let gradient = deep_field(row, "style", "gradient") {
                                kept.gradient = text_field(gradient, "kind")
                                kept.gradient_x0 = number_at(gradient, "from", 0)
                                kept.gradient_y0 = number_at(gradient, "from", 1)
                                kept.gradient_x1 = number_at(gradient, "to", 0)
                                kept.gradient_y1 = number_at(gradient, "to", 1)
                                if let stops = object_field(gradient, "stops") {
                                    kept.gradient_offsets = .some(stop_numbers(stops))
                                    kept.gradient_colours = .some(stop_colours(stops))
                                }
                            }
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
                if let media = object_field(opened, "media"), entries = as_object(media) {
                    for id in entries.keys() {
                        if let spec = entries.get(id) {
                            let medium = made.media.insert(
                                Medium(
                                    key: id,
                                    label: text_field(spec, "label"),
                                    engine: text_field(spec, "engine"),
                                    description: text_field(spec, "description"),
                                    ground: text_field(spec, "ground"),
                                    palette: pigments_of(spec),
                                ),
                            )
                            let kept_params = params_of(spec)
                            for key in kept_params.keys() {
                                medium.params.put(key, kept_params.get(key) ?? 0.0)
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
        if kind == "text" {
            return .some(
                Mark(
                    name: text_field(row, "name"),
                    layer: layer,
                    kind: "text",
                    text: text_field(shape, "text"),
                    x: number_field(shape, "x") ?? 0.0,
                    y: number_field(shape, "y") ?? 0.0,
                    size: number_field(shape, "size") ?? 24.0,
                    colour: fill,
                    font: text_field(shape, "font"),
                    weight: int_field(shape, "weight"),
                    italic: flag_field(shape, "italic") ?? false,
                    align: text_field(shape, "align"),
                    line_height: number_field(shape, "line_height") ?? 0.0,
                    tracking: number_field(shape, "tracking") ?? 0.0,
                    text_path: text_field(shape, "path"),
                    opacity: opacity,
                ),
            )
        }
        if kind == "image" and text_field(shape, "src") != "" {
            let crop = crop_of(shape)
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
                    crop_x: crop.at(0) ?? 0.0,
                    crop_y: crop.at(1) ?? 0.0,
                    crop_width: crop.at(2) ?? 1.0,
                    crop_height: crop.at(3) ?? 1.0,
                    straighten: number_field(shape, "straighten") ?? 0.0,
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

/// Puts a layer in the folder `group`, or takes it out when that is empty.
action set_layer_group(l: Layer, group: Text) (
    icon: .folder,
    label: "Put the layer in a folder",
    did: "moved a layer to a folder",
) {
    l.group = group.trim()
}

/// Holds a layer to the paint of the one under it, or lets it stand free.
action clip_layer(a: Artwork, layer: Layer, clip: Bool = not layer.clip) (
    icon: .scissors,
    label: "Clip the layer",
    did: "clipped a layer",
) {
    layer.clip = clip
}

/// Adds a mask to a layer: a layer of greys the layer is seen through. Its marks are made with
/// the brushes as any layer's are, light showing the layer and dark hiding it.
action add_mask(a: Artwork, layer: Layer) -> Layer (
    icon: .contrast,
    label: "Add a mask",
    did: "added a mask",
) {
    require layer.mask_of == "" "A mask is made for a layer, not for a mask"
    let made = a.layers.insert(
        Layer(name: free_layer_name(a, "{layer.name} mask"), mask_of: layer.name),
    )
    a.layers.move(made, after: layer)
    made
}

/// Sets a mask's grey before any mark, whether it hides where it is light, and whether it stays
/// put when its layer's marks move.
action set_mask(
    a: Artwork,
    layer: Layer,
    base: Float = layer.base,
    invert: Bool = layer.invert,
    unlinked: Bool = layer.unlinked,
) (label: "Set the mask", did: "set a mask") {
    require layer.mask_of != "" "Only a mask is set this way"
    require base >= 0.0 and base <= 1.0 "A mask's grey is 0 to 1"
    layer.base = base
    layer.invert = invert
    layer.unlinked = unlinked
}

/// Puts a mark in the folder `group`, or takes it out when that is empty. Marks of a group are
/// addressed, exported and shown together.
action group_mark(m: Mark, group: Text) (
    icon: .folder,
    label: "Put the mark in a folder",
    did: "moved a mark to a folder",
) {
    m.group = group.trim()
}

/// Adds a text mark: words set in the artwork's own fonts, at `x`, `y` (its first baseline) in
/// artwork units.
action add_text(
    a: Artwork,
    layer: Option<Layer>,
    text: Text,
    x: Float = 0.0,
    y: Float = 0.0,
    size: Float = 24.0,
    colour: Text = "#1d1b19",
    font: Text = "",
    weight: Int = 0,
    italic: Bool = false,
    align: Text = "",
    line_height: Float = 0.0,
    tracking: Float = 0.0,
    path: Text = "",
) -> Mark (icon: .type, label: "Add text", did: "added text") {
    require text.trim() != "" "There is no text to add"
    require size > 0.0 "Text is larger than nothing"
    require weight == 0 or weight >= 100 and weight <= 900 "A weight is 100 to 900"
    let on = if let chosen = layer { chosen } else { top_layer(a) }
    a.marks.insert(
        Mark(
            name: text,
            layer: on,
            kind: "text",
            text: text,
            x: x,
            y: y,
            size: size,
            colour: colour,
            font: font,
            weight: weight,
            italic: italic,
            align: align,
            line_height: line_height,
            tracking: tracking,
            text_path: path,
        ),
    )
}

/// Fills a mark with a gradient from `from` to `to`, its stops given as offsets 0 to 1 beside
/// their colours.
action fill_gradient(
    m: Mark,
    kind: Text = "linear",
    x0: Float,
    y0: Float,
    x1: Float,
    y1: Float,
    offsets: List<Float>,
    colours: List<Text>,
) (icon: .droplet, label: "Fill with a gradient", did: "filled a mark with a gradient") {
    require kind == "linear" or kind == "radial" "A gradient is linear or radial"
    require offsets.len() >= 2 and offsets.len() == colours.len() "A gradient has two or more stops, each with its colour"
    m.gradient = kind
    m.gradient_x0 = x0
    m.gradient_y0 = y0
    m.gradient_x1 = x1
    m.gradient_y1 = y1
    m.gradient_offsets = .some(offsets)
    m.gradient_colours = .some(colours)
}

/// Whether the artwork keeps a medium saved under `id`.
fn saved_medium(a: Artwork, id: Text) -> Bool {
    for m in a.media {
        if m.key == id {
            return true
        }
    }
    false
}

/// Paints a layer with a medium: `oil`, `watercolour`, a preset saved in the artwork by its id,
/// or empty for plain vector paint.
action set_medium(a: Artwork, layer: Layer, medium: Text) (
    icon: .droplet,
    label: "Paint the layer with a medium",
    did: "changed a layer's medium",
) {
    require medium == "" or medium == "oil" or medium == "watercolour" or saved_medium(a, medium) "A medium is oil, watercolour, or one saved in the artwork"
    require layer.mask_of == "" or medium == "" "A mask is painted in greys, with no medium"
    layer.medium = medium
}

/// Saves a medium in the artwork, so its layers name it and every device paints it the same. A
/// medium of the same id is replaced.
action save_medium(
    a: Artwork,
    id: Text,
    label: Text,
    engine: Text = "oil",
    description: Text = "",
    ground: Text = "",
    params: Map<Float> = Map<Float>(),
    palette: List<Pigment> = [],
) -> Medium (icon: .save, label: "Save a medium", did: "saved a medium") {
    require id.trim() != "" "A medium has an id"
    require engine == "oil" or engine == "watercolour" "A medium is oil or watercolour"
    if let known = a.media.find(m -> m.key == id) {
        known.label = label
        known.engine = engine
        known.description = description
        known.ground = ground
        let held = known.params.keys()
        for key in held {
            known.params.remove(key)
        }
        for key in params.keys() {
            known.params.put(key, params.get(key) ?? 0.0)
        }
        known.palette = palette
        return known
    }
    let made = a.media.insert(
        Medium(
            key: id,
            label: label,
            engine: engine,
            description: description,
            ground: ground,
            palette: palette,
        ),
    )
    for key in params.keys() {
        made.params.put(key, params.get(key) ?? 0.0)
    }
    made
}

destructive action remove_medium(a: Artwork, m: Medium) (icon: .trash, label: "Remove a medium") {
    for layer in a.layers.filter(l -> l.medium == m.key) {
        layer.medium = ""
    }
    a.media.remove(m)
}

/// Paints a mark into what is still wet, or over dry paint.
action wet_mark(m: Mark, wet: Bool = not m.wet) (
    icon: .droplet,
    label: "Paint into wet",
    did: "changed how a mark meets the wet paint",
) {
    m.wet = wet
}

/// How a layer's medium reads: plain paint when it has none.
fn medium_said(medium: Text) -> Text {
    if medium == "" { "plain paint" } else { medium }
}

/// Clears a mark's gradient, leaving the plain colour.
action clear_gradient(m: Mark) (label: "Clear the gradient") {
    m.gradient = ""
    m.gradient_offsets = .none
    m.gradient_colours = .none
}

/// Sets a text mark's words, its type and its setting. A parameter left out stays as it is.
action set_text(
    m: Mark,
    text: Text = m.text,
    size: Float = m.size,
    colour: Text = m.colour,
    font: Text = m.font,
    weight: Int = m.weight,
    italic: Bool = m.italic,
    align: Text = m.align,
    line_height: Float = m.line_height,
    tracking: Float = m.tracking,
    path: Text = m.text_path,
) (icon: .type, label: "Set the text", did: "changed text") {
    require m.kind == "text" "Only a text mark is set this way"
    require size > 0.0 "Text is larger than nothing"
    m.text = text
    m.size = size
    m.colour = colour
    m.font = font
    m.weight = weight
    m.italic = italic
    m.align = align
    m.line_height = line_height
    m.tracking = tracking
    m.text_path = path
}

/// The shape a person last drew, which the gradient fills.
fn last_shape(a: Artwork) -> Option<Mark> {
    a.marks.filter(m -> m.kind == "path").last()
}

/// The text a person last placed, which the text controls set.
fn last_text(a: Artwork) -> Option<Mark> {
    a.marks.filter(m -> m.kind == "text").last()
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
            group: layer.group,
            mask_of: layer.mask_of,
            base: layer.base,
            invert: layer.invert,
            clip: layer.clip,
            unlinked: layer.unlinked,
        ),
    )
    for m in a.marks.filter(m -> m.layer == layer) {
        let copy = a.marks.insert(
            Mark(
                name: m.name,
                layer: made,
                kind: m.kind,
                group: m.group,
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
                blend: m.blend,
                crop_x: m.crop_x,
                crop_y: m.crop_y,
                crop_width: m.crop_width,
                crop_height: m.crop_height,
                straighten: m.straighten,
                flip_h: m.flip_h,
                flip_v: m.flip_v,
                exposure: m.exposure,
                contrast: m.contrast,
                highlights: m.highlights,
                shadows: m.shadows,
                saturation: m.saturation,
                vibrance: m.vibrance,
                hue: m.hue,
                temperature: m.temperature,
                tint: m.tint,
                grayscale: m.grayscale,
                invert: m.invert,
                blur: m.blur,
                sharpen: m.sharpen,
                vignette: m.vignette,
                gradient: m.gradient,
                gradient_x0: m.gradient_x0,
                gradient_y0: m.gradient_y0,
                gradient_x1: m.gradient_x1,
                gradient_y1: m.gradient_y1,
                gradient_offsets: m.gradient_offsets,
                gradient_colours: m.gradient_colours,
                text: m.text,
                font: m.font,
                weight: m.weight,
                italic: m.italic,
                align: m.align,
                line_height: m.line_height,
                tracking: m.tracking,
                text_path: m.text_path,
                wet: m.wet,
            ),
        )
        for key in m.params.keys() {
            copy.params.put(key, m.params.get(key) ?? 0.0)
        }
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

/// Shows part of the picture: the frame `x`, `y`, `width`, `height` of its own pixels, each a
/// fraction of them. The picture stays put and the frame follows, so a crop is an edit of the
/// mark and never of the pixels.
action crop_picture(
    m: Mark,
    x: Float = 0.0,
    y: Float = 0.0,
    width: Float = 1.0,
    height: Float = 1.0,
) (icon: .crop, label: "Crop the picture", did: "cropped a picture") {
    require m.kind == "image" "Only a picture is cropped"
    require x >= 0.0 and y >= 0.0 "A crop starts inside the picture"
    require width > 0.0 and height > 0.0 "A crop has a width and a height"
    require x + width <= 1.0 and y + height <= 1.0 "A crop stays inside the picture"
    m.crop_x = x
    m.crop_y = y
    m.crop_width = width
    m.crop_height = height
}

/// Turns the picture inside its frame, in degrees, enlarging it so no corner shows.
action straighten_picture(m: Mark, degrees: Float) (
    icon: .rotate_cw,
    label: "Straighten the picture",
    did: "straightened a picture",
) {
    require m.kind == "image" "Only a picture is straightened"
    require degrees >= -180.0 and degrees <= 180.0 "A turn is -180 to 180 degrees"
    m.straighten = degrees
}

/// Mirrors the picture across its middle.
action flip_picture(
    m: Mark,
    horizontal: Bool = true,
    vertical: Bool = false,
    flip_h: Bool = not m.flip_h,
    flip_v: Bool = not m.flip_v,
) (label: "Flip the picture", did: "flipped a picture") {
    require m.kind == "image" "Only a picture is flipped"
    if horizontal {
        m.flip_h = flip_h
    }
    if vertical {
        m.flip_v = flip_v
    }
}

/// Sets how a mark mixes with what is under it.
action blend_mark(m: Mark, blend: Text) (
    icon: .layers,
    label: "Change the blend",
    did: "changed a mark's blend",
) {
    m.blend = if blend == "normal" { "" } else { blend }
}

/// Grades a mark without touching its pixels: exposure, contrast and the rest are kept on the
/// mark and applied wherever the artwork is shown or exported. A parameter left out stays as it
/// is, so an agent changes one step at a time.
action adjust_mark(
    m: Mark,
    exposure: Float = m.exposure,
    contrast: Float = m.contrast,
    highlights: Float = m.highlights,
    shadows: Float = m.shadows,
    saturation: Float = m.saturation,
    vibrance: Float = m.vibrance,
    hue: Float = m.hue,
    temperature: Float = m.temperature,
    tint: Float = m.tint,
    grayscale: Bool = m.grayscale,
    invert: Bool = m.invert,
    blur: Float = m.blur,
    sharpen: Float = m.sharpen,
    vignette: Float = m.vignette,
) (icon: .sliders_horizontal, label: "Adjust the mark", did: "adjusted a mark") {
    require exposure >= -5.0 and exposure <= 5.0 "Exposure is -5 to 5 stops"
    require hue >= -180.0 and hue <= 180.0 "Hue is -180 to 180 degrees"
    require blur >= 0.0 and blur <= 200.0 "A blur is 0 to 200 units"
    require sharpen >= 0.0 and sharpen <= 1.0 "A sharpen is 0 to 1"
    m.exposure = exposure
    m.contrast = contrast
    m.highlights = highlights
    m.shadows = shadows
    m.saturation = saturation
    m.vibrance = vibrance
    m.hue = hue
    m.temperature = temperature
    m.tint = tint
    m.grayscale = grayscale
    m.invert = invert
    m.blur = blur
    m.sharpen = sharpen
    m.vignette = vignette
}

/// A picture with those steps applied to its pixels, as a new file: what a person or an agent
/// asks for when the graded picture itself is wanted, rather than the artwork that shows it.
action grade_picture(
    file: File,
    exposure: Float = 0.0,
    contrast: Float = 0.0,
    highlights: Float = 0.0,
    shadows: Float = 0.0,
    saturation: Float = 0.0,
    vibrance: Float = 0.0,
    hue: Float = 0.0,
    temperature: Float = 0.0,
    tint: Float = 0.0,
    grayscale: Bool = false,
    invert: Bool = false,
    blur: Float = 0.0,
    sharpen: Float = 0.0,
    vignette: Float = 0.0,
) -> Option<File> uses image (icon: .sliders_horizontal, label: "Grade a picture") {
    let ask = json.encode(
        GradeAsk(
            exposure: exposure,
            contrast: contrast,
            highlights: highlights,
            shadows: shadows,
            saturation: saturation,
            vibrance: vibrance,
            hue: hue,
            temperature: temperature,
            tint: tint,
            grayscale: grayscale,
            invert: invert,
            blur: blur,
            sharpen: sharpen,
            vignette: vignette,
        ),
    )
    match canvas.grade(file, ask) {
        .ok(graded) => .some(graded),
        .err(why) => .none,
    }
}

/// What a picture's grade asks the device's drawing engine for.
type GradeAsk {
    exposure: Float
    contrast: Float
    highlights: Float
    shadows: Float
    saturation: Float
    vibrance: Float
    hue: Float
    temperature: Float
    tint: Float
    grayscale: Bool
    invert: Bool
    blur: Float
    sharpen: Float
    vignette: Float
}

/// An adjustment step, absent when it does nothing.
fn optional(value: Float) -> Option<Float> {
    if value == 0.0 {
        return .none
    }
    .some(value)
}

/// What a magic wand asks the device's drawing engine for: the area like a colour the person
/// pointed at, answered in the artwork's units through the picture's frame.
type WandAsk {
    at: Option<List<Float>> = .none
    colour: Option<Text> = .none
    tolerance: Option<Float> = .none
    everywhere: Option<Bool> = .none
    frame: Option<List<Float>> = .none
}

/// Selects what the colour at `at` touches in the artwork's picture, or that colour everywhere:
/// the magic wand. The area comes back as a selection the fill, mask and outline use.
action select_like(
    a: Artwork,
    at: List<Float> = [],
    colour: Text = "",
    tolerance: Float = 0.15,
    everywhere: Bool = false,
) -> Option<Text> uses image (icon: .wand_sparkles, label: "Select what a colour touches") {
    require at.len() >= 2 or colour != "" "Point at a colour, or name one"
    require tolerance >= 0.0 and tolerance <= 1.0 "A tolerance is 0 to 1"
    if let p = last_picture(a) {
        let ask = WandAsk(
            at: optional_at(at),
            colour: optional_text(colour),
            tolerance: .some(tolerance),
            everywhere: .some(everywhere),
            frame: .some([p.x, p.y, p.width, p.height]),
        )
        match canvas.wand(p.src, json.encode(ask)) {
            .ok(d) => {
                a.selection = .some(Area(d: d, feather: 0.0))
                return .some(d)
            }
            .err(why) => .none,
        }
    } else {
        a.notice = "Place a picture first: the wand reads a picture's colours"
        return .none
    }
}

/// What a subject or a refined edge asks the device's drawing engine for.
type FindAsk {
    subject: Option<Bool> = .none
    refine: Option<Float> = .none
    at: Option<List<Float>> = .none
    colour: Option<Text> = .none
    tolerance: Option<Float> = .none
    layer: Option<Text> = .none
    feather: Option<Float> = .none
}

/// Selects the picture's subject, found by the on-device model the person set up, with its edge
/// fitted to the picture's own edges over `refine` units where that is asked.
action select_subject(a: Artwork, refine: Float = 0.0) -> Option<Text> uses image (
    icon: .sparkles,
    label: "Select the subject",
) {
    require refine >= 0.0 and refine <= 200.0 "A refine is 0 to 200 units"
    let ask = FindAsk(subject: .some(true), refine: optional(refine), feather: .some(0.0))
    match canvas.find(artwork_json(a), json.encode(ask), picture_addresses(a)) {
        .ok(d) => {
            a.selection = .some(Area(d: d, feather: 0.0))
            return .some(d)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// Refines the selection's edge to the picture's own edges over `refine` units, keeping fur and
/// hair soft.
action refine_selection(a: Artwork, refine: Float = 6.0) -> Option<Text> uses image (
    icon: .circle_dashed,
    label: "Refine the selection",
) {
    require a.selection != .none "Select an area first"
    require refine > 0.0 and refine <= 200.0 "A refine is over 0 and at most 200 units"
    let ask = FindAsk(refine: .some(refine), feather: .some(0.0))
    match canvas.find(artwork_json(a), json.encode(ask), picture_addresses(a)) {
        .ok(d) => {
            a.selection = .some(Area(d: d, feather: 0.0))
            return .some(d)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

fn optional_text(value: Text) -> Option<Text> {
    if value == "" {
        return .none
    }
    .some(value)
}

fn optional_at(at: List<Float>) -> Option<List<Float>> {
    if at.len() >= 2 {
        return .some(at)
    }
    .none
}

/// The picture a person last placed in the artwork, which the picture controls adjust.
fn last_picture(a: Artwork) -> Option<Mark> {
    a.marks.filter(m -> m.kind == "image").last()
}

/// The marks standing on `layer`, bottom to top.
fn marks_on(a: Artwork, layer: Layer) -> List<Mark> {
    a.marks.filter(m -> m.layer == layer)
}

/// The blends a mark mixes with, by the names the records keep.
fn blends() -> List<Text> {
    [
        "normal",
        "multiply",
        "screen",
        "overlay",
        "darken",
        "lighten",
        "color-dodge",
        "color-burn",
        "hard-light",
        "soft-light",
        "difference",
        "exclusion",
        "hue",
        "saturation",
        "color",
        "luminosity",
    ]
}

/// What a combine asks the device's drawing engine for.
type CombineAsk {
    ids: List<Text>
    op: Text
}

/// What a measurement asks for: the marks or groups it weighs.
type MeasureAsk {
    ids: List<Text>
}

/// What a comparison with the reference asks for.
type CompareAsk {
    region: Option<List<Float>> = .none
    mode: Option<Text> = .none
    size: Option<Int> = .none
}

/// Combines the marks into one path, in the bottom one's place and colour: shapes united,
/// subtracted, intersected or excluded. The engine works out the outline; the artwork keeps one
/// mark where there were several.
action combine_marks(a: Artwork, marks: List<Mark>, op: Text = "union") -> Option<Mark> uses image (
    icon: .layers,
    label: "Combine shapes",
    did: "combined shapes",
) {
    require marks.len() >= 2 "Name two or more marks to combine"
    require op == "union" or op == "subtract" or op == "intersect" or op == "exclude" "Shapes unite, subtract, intersect or exclude"
    let ids = marks.map(m -> "mark-{a.marks.index_of(m) ?? 0}")
    match canvas.combine(artwork_json(a), json.encode(CombineAsk(ids: ids, op: op))) {
        .ok(answer) => match json.parse(answer) {
            .ok(held) => {
                let d = text_field(held, "d")
                if d == "" {
                    a.notice = "Those shapes leave nothing"
                    return .none
                }
                // The bottom of them takes the combined outline; the others go.
                let kept = if let found = a.marks
                        .filter(m -> marks.find(n -> n == m) != .none)
                        .first() {
                    found
                } else if let named = marks.first() {
                    named
                } else {
                    a.marks.insert(Mark(layer: top_layer(a)))
                }
                let fill = text_field(held, "fill")
                for m in marks {
                    if m != kept { a.marks.remove(m) }
                }
                kept.kind = "path"
                kept.d = d
                if fill != "" {
                    kept.colour = fill
                }
                a.notice = ""
                return .some(kept)
            }
            .err(_) => {
                a.notice = "The combined shape cannot be read"
                return .none
            }
        },
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// Weighs the marks' ink as the eye sees it: each one's ink box, ink area, centre of mass and
/// its offset from the box's middle, so two icons balance when their areas and offsets match.
/// The answer is the engine's JSON; the window says it in words.
action measure_marks(a: Artwork, marks: List<Mark>) -> Option<Text> uses image (
    icon: .sliders_horizontal,
    label: "Measure ink",
) {
    require marks.len() > 0 "Name the marks to measure"
    let ids = marks.map(m -> "mark-{a.marks.index_of(m) ?? 0}")
    match canvas.measure(artwork_json(a), json.encode(MeasureAsk(ids: ids)), picture_addresses(a)) {
        .ok(answer) => {
            a.notice = measure_said(answer)
            return .some(answer)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// What a measurement weighs, in a line a person reads.
fn measure_said(answer: Text) -> Text {
    match json.parse(answer) {
        .ok(rows) => match as_array(rows) {
            .some(entries) => {
                let said = entries.map(row -> measure_line(row))
                if said.len() > 0 {
                    said.join("; ")
                } else { "Nothing of those marks puts down ink" }
            }
            .none => "Measured",
        },
        .err(_) => "Measured",
    }
}

/// One measured mark, in a line.
fn measure_line(row: json.Value) -> Text {
    let id = text_field(row, "id")
    let area = number_field(row, "ink_area") ?? 0.0
    let across = number_at(row, "offset", 0)
    let down = number_at(row, "offset", 1)
    "{id}: {area} units of ink, {across} by {down} from the middle"
}

/// Sets the picture to work from, fitted inside the artwork and centred. It is not part of the
/// work: comparing shows the artwork beside, over or against it, and export never includes it.
action set_reference(a: Artwork, file: File, opacity: Float = 1.0) uses image (
    icon: .image,
    label: "Set the reference",
    did: "changed the reference picture",
) {
    require file.url != "" "A reference is a picture from a file"
    require opacity >= 0.0 and opacity <= 1.0 "A reference is 0 to 1 opaque"
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
    a.reference = .some(
        Reference(
            image: file.url,
            x: (a.width - width) / 2.0,
            y: (a.height - height) / 2.0,
            width: width,
            height: height,
            opacity: opacity,
        ),
    )
}

/// Takes the reference picture away.
action clear_reference(a: Artwork) (
    label: "Clear the reference",
    did: "cleared the reference picture",
) {
    a.reference = .none
}

/// Shows the artwork beside (`side-by-side`), over (`overlay`) or against (`difference`) the
/// reference picture, as a new file, with how far apart they are.
action compare_reference(
    a: Artwork,
    mode: Text = "side-by-side",
    size: Int = 640,
) -> Option<File> uses image (icon: .eye, label: "Compare with the reference") {
    require a.reference != .none "Set a reference picture first"
    require mode == "side-by-side" or mode == "overlay" or mode == "difference" "A comparison is side-by-side, overlay or difference"
    require size >= 64 and size <= 1024 "A comparison is 64 to 1024 pixels a side"
    let ask = CompareAsk(mode: .some(mode), size: .some(size))
    match canvas.compare(artwork_json(a), json.encode(ask), picture_addresses(a)) {
        .ok(file) => {
            a.comparison = file.url
            a.notice = ""
            return .some(file)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
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
            media: a.media.map(m -> medium_json(m)),
            selection: area_json(a),
            reference: reference_json(a),
        ),
    )
}

fn reference_json(a: Artwork) -> Option<ReferenceJson> {
    if let reference = a.reference {
        return .some(
            ReferenceJson(
                image: reference.image,
                frame: if reference.width > 0.0 and reference.height > 0.0 {
                    .some([reference.x, reference.y, reference.width, reference.height])
                } else { .none },
                opacity: reference.opacity,
                visible: reference.visible,
            ),
        )
    }
    .none
}

fn layer_json(l: Layer) -> LayerJson {
    LayerJson(
        name: l.name,
        medium: l.medium,
        opacity: l.opacity,
        visible: l.visible,
        locked: l.locked,
        texture: l.texture,
        group: l.group,
        mask_of: l.mask_of,
        base: l.base,
        invert: l.invert,
        clip: l.clip,
        unlinked: l.unlinked,
    )
}

fn medium_json(m: Medium) -> MediumJson {
    MediumJson(
        id: m.key,
        label: m.label,
        engine: m.engine,
        description: m.description,
        ground: m.ground,
        params: m.params,
        palette: m.palette.map(
            p -> PigmentJson(
                name: p.name,
                colour: p.colour,
                granulation: p.granulation,
                strength: p.strength,
            ),
        ),
    )
}

/// The pigments a saved medium carries, read from the engine's JSON.
fn pigments_of(spec: json.Value) -> List<Pigment> {
    if let held = object_field(spec, "palette"), items = as_array(held) {
        items.map(
            p -> Pigment(
                name: text_field(p, "name"),
                colour: text_field(p, "colour"),
                granulation: number_field(p, "granulation") ?? 0.0,
                strength: number_field(p, "strength") ?? 1.0,
            ),
        )
    } else { [] }
}

fn mark_json(m: Mark) -> MarkJson {
    MarkJson(
        name: m.name,
        layer: layer_json(m.layer),
        kind: m.kind,
        group: m.group,
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
        blend: blend_of(m.blend),
        crop_x: m.crop_x,
        crop_y: m.crop_y,
        crop_width: m.crop_width,
        crop_height: m.crop_height,
        straighten: m.straighten,
        flip_h: m.flip_h,
        flip_v: m.flip_v,
        exposure: m.exposure,
        contrast: m.contrast,
        highlights: m.highlights,
        shadows: m.shadows,
        saturation: m.saturation,
        vibrance: m.vibrance,
        hue: m.hue,
        temperature: m.temperature,
        tint: m.tint,
        grayscale: m.grayscale,
        invert: m.invert,
        blur: m.blur,
        sharpen: m.sharpen,
        vignette: m.vignette,
        gradient_kind: m.gradient,
        gradient_x0: m.gradient_x0,
        gradient_y0: m.gradient_y0,
        gradient_x1: m.gradient_x1,
        gradient_y1: m.gradient_y1,
        gradient_offsets: m.gradient_offsets ?? [],
        gradient_colours: m.gradient_colours ?? [],
        text: m.text,
        font: m.font,
        weight: m.weight,
        italic: m.italic,
        align: m.align,
        line_height: m.line_height,
        tracking: m.tracking,
        text_path: m.text_path,
        wet: m.wet,
        params: m.params,
    )
}

/// The blend a mark mixes with: `normal` for the empty name the records keep.
fn blend_of(blend: Text) -> Text {
    if blend == "" { "normal" } else { blend }
}

fn area_json(a: Artwork) -> Option<AreaJson> {
    if let area = a.selection {
        return .some(AreaJson(d: area.d, feather: area.feather))
    }
    .none
}

fn picture_addresses(a: Artwork) -> List<Text> {
    let addresses = a.pictures.map(p -> p.url)
    if let reference = a.reference {
        if reference.image != "" and addresses.find(u -> u == reference.image) == .none {
            return addresses + [reference.image]
        }
    }
    addresses
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

fn nested_flag(value: json.Value, of: Text, key: Text) -> Option<Bool> {
    if let held = object_field(value, of) { flag_field(held, key) } else { .none }
}

/// Whether the picture stands flipped on the axis its transform's `at`th number scales:
/// 0 across, 3 down.
fn axis_negative(row: json.Value, at: Int) -> Bool {
    if let held = object_field(row, "transform"), items = as_array(held) {
        if let value = items.at(at) { (as_number(value) ?? 1.0) < 0.0 } else { false }
    } else { false }
}

/// The numbers of the JSON object `key` of the object `of` holds.
fn nested_params(value: json.Value, of: Text, key: Text) -> Map<Float> {
    if let held = object_field(value, of) { params_of(held) } else { Map<Float>() }
}

/// The numbers of a JSON object's `params`.
fn params_of(value: json.Value) -> Map<Float> {
    let params: Map<Float> = Map<Float>()
    if let fields = object_field(value, "params"), entries = as_object(fields) {
        for key in entries.keys() {
            if let held = entries.get(key) { params.put(key, as_number(held) ?? 0.0) }
        }
    }
    params
}

fn int_field(value: json.Value, key: Text) -> Int {
    if let held = number_field(value, key) { held.round() } else { 0 }
}

fn deep_field(value: json.Value, of: Text, key: Text) -> Option<json.Value> {
    if let held = object_field(value, of) { object_field(held, key) } else { .none }
}

/// The `at`th number of the array `key` of the object `of` holds.
fn number_at(value: json.Value, key: Text, at: Int) -> Float {
    if let held = object_field(value, key), items = as_array(held) {
        if let item = items.at(at) { as_number(item) ?? 0.0 } else { 0.0 }
    } else { 0.0 }
}

fn pair_number(pair: json.Value, at: Int) -> Float {
    if let items = as_array(pair) {
        if let item = items.at(at) { as_number(item) ?? 0.0 } else { 0.0 }
    } else { 0.0 }
}

fn pair_text(pair: json.Value, at: Int) -> Text {
    if let items = as_array(pair) {
        if let item = items.at(at) { as_text(item) ?? "" } else { "" }
    } else { "" }
}

fn stop_numbers(stops: json.Value) -> List<Float> {
    if let items = as_array(stops) { items.map(pair -> pair_number(pair, 0)) } else { [] }
}

fn stop_colours(stops: json.Value) -> List<Text> {
    if let items = as_array(stops) { items.map(pair -> pair_text(pair, 1)) } else { [] }
}

/// The picture's crop fractions the file kept, or the whole picture.
fn crop_of(shape: json.Value) -> List<Float> {
    if let held = object_field(shape, "crop") { numbers_of(held) } else { [] }
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
    std.page(a, canvas: true, fill: true) {
        header: {
            artwork_head(a, tool, brush, size, colour, show_layers, zoom, pan_x, pan_y)
        }
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
        ui.ToolbarButton("Wand", icon: .wand_sparkles, toggled: tool == "wand") {
            on press: set(tool, "wand")
        }
        ui.ToolbarButton("Text", icon: .type, toggled: tool == "text") {
            on press: set(tool, "text")
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
        ui.ToolbarButton("Set the reference", icon: .image) {
            on press: file.choose(
                set_reference(a),
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
        if let layer = chosen {
            ui.MenuButton("Medium: {medium_said(layer.medium)}", icon: .droplet) {
                ui.MenuItem("Plain paint") {
                    on choose: set_medium(a, layer, "")
                }
                ui.MenuItem("Oil") {
                    on choose: set_medium(a, layer, "oil")
                }
                ui.MenuItem("Watercolour") {
                    on choose: set_medium(a, layer, "watercolour")
                }
                for m in a.media {
                    ui.MenuItem(m.label) {
                        on choose: set_medium(a, layer, m.key)
                    }
                }
            }
            let shapes = marks_on(a, layer)
            if shapes.len() >= 2 {
                ui.MenuButton("Combine", icon: .layers) {
                    ui.MenuItem("Unite") {
                        on choose: combine_marks(a, shapes, "union")
                    }
                    ui.MenuItem("Subtract") {
                        on choose: combine_marks(a, shapes, "subtract")
                    }
                    ui.MenuItem("Intersect") {
                        on choose: combine_marks(a, shapes, "intersect")
                    }
                    ui.MenuItem("Exclude") {
                        on choose: combine_marks(a, shapes, "exclude")
                    }
                }
            }
            let marks = marks_on(a, layer)
            if marks.len() > 0 {
                ui.Button("Measure", icon: .sliders_horizontal) {
                    on press: measure_marks(a, marks)
                }
            }
        }
        if a.media.len() > 0 {
            ui.MenuButton("Saved media", icon: .save) {
                for m in a.media {
                    ui.MenuItem("Remove {m.label}") {
                        on choose: remove_medium(a, m)
                    }
                }
            }
        }
        ui.Button("Add layer", icon: .plus, role: .plain, size: .small) {
            on press: add_layer(a, "")
        }
        if let p = last_picture(a) {
            picture_panel(p)
        }
        if let t = last_text(a) {
            text_panel(t)
        }
    }
}

/// The text a person last placed: its words, its size, its colour, its weight and its slope.
view text_panel(t: Mark) {
    ui.Section("Text") {
        ui.TextInput(bind: t.text)
        ui.Text("Size", role: .caption, secondary: true)
        ui.Slider(bind: t.size, label: "Size", min: 4.0, max: 400.0)
        ui.ColorPicker(bind: t.colour, swatches: inks(), columns: 7, label: "Text colour")
        row(gap: .tight) {
            ui.Button("Light", toggled: t.weight == 300) {
                on press: set_text(t, weight: 300)
            }
            ui.Button("Regular", toggled: t.weight == 400) {
                on press: set_text(t, weight: 400)
            }
            ui.Button("Bold", toggled: t.weight == 700) {
                on press: set_text(t, weight: 700)
            }
            ui.Button(if t.italic { "Upright" } else { "Italic" }) {
                on press: set_text(t, italic: not t.italic)
            }
        }
    }
}

/// The picture a person last placed: its crop, its turn and its grades, each kept on the mark and
/// applied wherever the artwork is drawn.
view picture_panel(p: Mark) {
    ui.Section("Picture") {
        row(gap: .tight, align: .center) {
            ui.Button("Flip across", icon: .rotate_cw) {
                on press: flip_picture(p, true, false)
            }
            ui.Button("Flip down") {
                on press: flip_picture(p, false, true)
            }
            ui.MenuButton("Crop", icon: .crop) {
                ui.MenuItem("Whole picture") {
                    on choose: crop_picture(p, 0.0, 0.0, 1.0, 1.0)
                }
                ui.MenuItem("Centre half") {
                    on choose: crop_picture(p, 0.25, 0.25, 0.5, 0.5)
                }
                ui.MenuItem("Centre strip") {
                    on choose: crop_picture(p, 0.0, 0.25, 1.0, 0.5)
                }
            }
        }
        ui.MenuButton("Blend: {blend_of(p.blend)}", icon: .layers) {
            for blend in blends() {
                ui.MenuItem(blend) {
                    on choose: blend_mark(p, blend)
                }
            }
        }
        ui.Text("Turn", role: .caption, secondary: true)
        ui.Slider(bind: p.straighten, label: "Turn", min: -45.0, max: 45.0)
        ui.Text("Exposure", role: .caption, secondary: true)
        ui.Slider(bind: p.exposure, label: "Exposure", min: -3.0, max: 3.0)
        ui.Text("Contrast", role: .caption, secondary: true)
        ui.Slider(bind: p.contrast, label: "Contrast", min: -1.0, max: 1.0)
        ui.Text("Highlights", role: .caption, secondary: true)
        ui.Slider(bind: p.highlights, label: "Highlights", min: -1.0, max: 1.0)
        ui.Text("Shadows", role: .caption, secondary: true)
        ui.Slider(bind: p.shadows, label: "Shadows", min: -1.0, max: 1.0)
        ui.Text("Saturation", role: .caption, secondary: true)
        ui.Slider(bind: p.saturation, label: "Saturation", min: -1.0, max: 1.0)
        ui.Text("Vibrance", role: .caption, secondary: true)
        ui.Slider(bind: p.vibrance, label: "Vibrance", min: -1.0, max: 1.0)
        ui.Text("Hue", role: .caption, secondary: true)
        ui.Slider(bind: p.hue, label: "Hue", min: -180.0, max: 180.0)
        ui.Text("Warmth", role: .caption, secondary: true)
        ui.Slider(bind: p.temperature, label: "Warmth", min: -1.0, max: 1.0)
        ui.Text("Tint", role: .caption, secondary: true)
        ui.Slider(bind: p.tint, label: "Tint", min: -1.0, max: 1.0)
        ui.Text("Blur", role: .caption, secondary: true)
        ui.Slider(bind: p.blur, label: "Blur", min: 0.0, max: 40.0)
        ui.Text("Sharpen", role: .caption, secondary: true)
        ui.Slider(bind: p.sharpen, label: "Sharpen", min: 0.0, max: 1.0)
        ui.Text("Vignette", role: .caption, secondary: true)
        ui.Slider(bind: p.vignette, label: "Vignette", min: -1.0, max: 1.0)
        row(gap: .tight) {
            ui.Button(if p.grayscale { "Colour" } else { "Black and white" }) {
                on press: adjust_mark(p, grayscale: not p.grayscale)
            }
            ui.Button(if p.invert { "Un-invert" } else { "Invert" }) {
                on press: adjust_mark(p, invert: not p.invert)
            }
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
        if let shape = last_shape(a) {
            ui.MenuButton("Gradient", icon: .droplet) {
                ui.MenuItem("Linear, into the paint") {
                    on choose: fill_gradient(
                        shape,
                        "linear",
                        0.0,
                        0.0,
                        a.width,
                        a.height,
                        [0.0, 1.0],
                        [colour, "{colour}00"],
                    )
                }
                ui.MenuItem("Radial, from the centre") {
                    on choose: fill_gradient(
                        shape,
                        "radial",
                        a.width / 2.0,
                        a.height / 2.0,
                        a.width,
                        a.height,
                        [0.0, 1.0],
                        [colour, "{colour}00"],
                    )
                }
                ui.MenuItem("Clear the gradient") {
                    on choose: clear_gradient(shape)
                }
            }
        }
        if a.reference != .none {
            ui.MenuButton("Reference", icon: .image) {
                ui.MenuItem("Side by side") {
                    on choose: compare_reference(a, "side-by-side", 640)
                }
                ui.MenuItem("Overlay") {
                    on choose: compare_reference(a, "overlay", 640)
                }
                ui.MenuItem("Difference") {
                    on choose: compare_reference(a, "difference", 640)
                }
                ui.MenuItem("Clear the reference") {
                    on choose: clear_reference(a)
                }
            }
        }
        if a.comparison != "" {
            ui.ImageViewer(a.comparison, alt: "The artwork against its reference", aspect: 1.5)
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
    if tool == "wand" {
        select_like(a, point, "", 0.15, false)
        return
    }
    if tool == "text" {
        if let x = point.at(0), y = point.at(1) {
            add_text(a, .none, "Text", x, y, 32.0, colour, "", 0, false, "", 0.0, 0.0)
        }
        return
    }
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
