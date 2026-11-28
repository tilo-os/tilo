app so.tilo.canvas "Canvas" (
    icon: .image,
    category: .graphics,
    tagline: "Draw and paint that stays editable",
    keywords: ["draw", "drawing", "paint", "sketch", "whiteboard", "art", "illustration", "canvas", "vector", "watercolour", "oil", "zeichnen", "malen", "skizze", "zeichenbrett", "leinwand", "tafel"],
    document: library.artworks,
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
    /// The marks a person picked, which combine and measure act on.
    picked: List<Mark>
    /// The picture to work from, which `compare_reference` shows the artwork against; it is not
    /// part of the work and never exported.
    reference: Option<Reference> = .none
    /// The picture of the artwork as last drawn, so the surface shows paint under the hand.
    look: Text = ""
    /// The artwork beside, over or against its reference as last compared, kept to look at.
    comparison: Text = ""
    /// What comparing two versions found, in a line a person reads.
    version_note: Text = ""
    /// The changed region of the version compared, as the artwork now stands, laid over the
    /// surface where it stands.
    version_overlay: Text = ""
    /// That region, `[x, y, width, height]` in artwork units.
    version_region: List<Float>
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
    /// Colour and tone adjustments of everything on the layer, applied after each mark's own.
    exposure: Float = 0.0
    contrast: Float = 0.0
    highlights: Float = 0.0
    shadows: Float = 0.0
    saturation: Float = 0.0
    vibrance: Float = 0.0
    hue: Float = 0.0
    temperature: Float = 0.0
    tint: Float = 0.0
    adjust_grayscale: Bool = false
    adjust_invert: Bool = false
    blur: Float = 0.0
    sharpen: Float = 0.0
    vignette: Float = 0.0
    /// Tone curve `[[in, out], …]`, 0 to 1.
    curve: List<List<Float>>
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
    /// An outline drawn round the mark: its colour, its width in units, and its dashes
    /// `[on, off, …]`. Empty or no width for none.
    stroke: Text = ""
    stroke_width: Float = 0.0
    dash: List<Float>
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
    /// An affine transform `[a, b, c, d, e, f]` applied to the mark's geometry: how it is
    /// moved, scaled and turned.
    transform: List<Float>
    /// The mark or group whose shape this mark is cut to; empty for uncut.
    clip: Text = ""
    /// Enlarge picture pixels with nearest-neighbour sampling.
    pixelated: Bool = false
    /// Film grain over the mark, 0 to 1.
    grain: Float = 0.0
    /// Soften the mark's edge outward over this many units.
    feather: Float = 0.0
    /// A shadow cast outside the mark, and one cast inside it.
    shadow_x: Float = 0.0
    shadow_y: Float = 0.0
    shadow_blur: Float = 0.0
    shadow_colour: Text = ""
    inner_shadow_x: Float = 0.0
    inner_shadow_y: Float = 0.0
    inner_shadow_blur: Float = 0.0
    inner_shadow_colour: Text = ""
    /// Tone curve `[[in, out], …]`, 0 to 1.
    curve: List<List<Float>>
}

/// The area selected for the next edit.
type Area {
    /// SVG path data in artwork units.
    d: Text
    /// How far its edge fades, in units.
    feather: Float = 0.0
    /// The soft edge a refined selection keeps: a picture, white where selected, and its frame.
    /// A mask made of the selection draws it.
    matte_src: Text = ""
    matte_x: Float = 0.0
    matte_y: Float = 0.0
    matte_width: Float = 0.0
    matte_height: Float = 0.0
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
    /// Draw only the selection's area, as the selection's outline encloses it.
    selection: Option<Bool> = .none
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
    invert_adjust: Bool
    blur: Float
    sharpen: Float
    vignette: Float
    curve: List<List<Float>>
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
    stroke: Text
    stroke_width: Float
    dash: List<Float>
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
    transform: List<Float>
    clip: Text
    pixelated: Bool
    grain: Float
    feather: Float
    shadow_x: Float
    shadow_y: Float
    shadow_blur: Float
    shadow_colour: Text
    inner_shadow_x: Float
    inner_shadow_y: Float
    inner_shadow_blur: Float
    inner_shadow_colour: Text
    curve: List<List<Float>>
}

type AreaJson {
    d: Text
    feather: Float
    matte: Option<MatteJson>
}

type MatteJson {
    src: Text
    rect: List<Float>
}

root library: Library

default view library for Library

view library(lib: Library) {
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            // The sizes an artwork starts at, as a print shop or a screen hands them out.
            ui.MenuButton("New artwork", icon: .plus) {
                ui.MenuItem("Screen, 1920 × 1080", key: "Ctrl+N") {
                    on choose: new_artwork(lib, "Untitled artwork", 1920.0, 1080.0)
                }
                ui.MenuItem("A4, 2480 × 3508") {
                    on choose: new_artwork(lib, "Untitled artwork", 2480.0, 3508.0)
                }
                ui.MenuItem("Letter, 2550 × 3300") {
                    on choose: new_artwork(lib, "Untitled artwork", 2550.0, 3300.0)
                }
                ui.MenuItem("Square, 2048 × 2048") {
                    on choose: new_artwork(lib, "Untitled artwork", 2048.0, 2048.0)
                }
                ui.MenuItem("Poster, 3600 × 5400") {
                    on choose: new_artwork(lib, "Untitled artwork", 3600.0, 5400.0)
                }
                ui.MenuItem("Icon, 1024 × 1024") {
                    on choose: new_artwork(lib, "Untitled artwork", 1024.0, 1024.0)
                }
                ui.MenuSeparator()
                ui.MenuItem("Sprite, 32 × 32") {
                    on choose: new_artwork(lib, "Untitled artwork", 32.0, 32.0)
                }
                ui.MenuItem("Sprite, 64 × 64") {
                    on choose: new_artwork(lib, "Untitled artwork", 64.0, 64.0)
                }
                ui.MenuItem("Scene, 128 × 128") {
                    on choose: new_artwork(lib, "Untitled artwork", 128.0, 128.0)
                }
            }
            ui.Button("Open an artwork…", icon: .file_plus, key: "Ctrl+O") {
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
                            exposure: number_field(row, "exposure") ?? 0.0,
                            contrast: number_field(row, "contrast") ?? 0.0,
                            highlights: number_field(row, "highlights") ?? 0.0,
                            shadows: number_field(row, "shadows") ?? 0.0,
                            saturation: number_field(row, "saturation") ?? 0.0,
                            vibrance: number_field(row, "vibrance") ?? 0.0,
                            hue: number_field(row, "hue") ?? 0.0,
                            temperature: number_field(row, "temperature") ?? 0.0,
                            tint: number_field(row, "tint") ?? 0.0,
                            adjust_grayscale: flag_field(row, "grayscale") ?? false,
                            adjust_invert: flag_field(row, "invert_adjust") ?? false,
                            blur: number_field(row, "blur") ?? 0.0,
                            sharpen: number_field(row, "sharpen") ?? 0.0,
                            vignette: number_field(row, "vignette") ?? 0.0,
                            curve: curves_of(row),
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
                            let style = nested_object(row, "style")
                            let adjust = nested_object(style, "adjust")
                            kept.exposure = number_field(adjust, "exposure") ?? 0.0
                            kept.contrast = number_field(adjust, "contrast") ?? 0.0
                            kept.highlights = number_field(adjust, "highlights") ?? 0.0
                            kept.shadows = number_field(adjust, "shadows") ?? 0.0
                            kept.saturation = number_field(adjust, "saturation") ?? 0.0
                            kept.vibrance = number_field(adjust, "vibrance") ?? 0.0
                            kept.hue = number_field(adjust, "hue") ?? 0.0
                            kept.temperature = number_field(adjust, "temperature") ?? 0.0
                            kept.tint = number_field(adjust, "tint") ?? 0.0
                            kept.grayscale = flag_field(adjust, "grayscale") ?? false
                            kept.invert = flag_field(adjust, "invert") ?? false
                            kept.blur = number_field(adjust, "blur") ?? 0.0
                            kept.sharpen = number_field(adjust, "sharpen") ?? 0.0
                            kept.vignette = number_field(adjust, "vignette") ?? 0.0
                            kept.curve = curves_of(adjust)
                            kept.flip_h = axis_negative(row, 0)
                            kept.flip_v = axis_negative(row, 3)
                            kept.wet = flag_field(style, "wet") ?? false
                            kept.transform = numbers_at(row, "transform")
                            kept.clip = text_field(row, "clip")
                            kept.pixelated = flag_field(style, "pixelated") ?? false
                            kept.grain = number_field(style, "grain") ?? 0.0
                            kept.feather = number_field(style, "feather") ?? 0.0
                            if let shadow = object_field(style, "shadow") {
                                let offset = numbers_at(shadow, "offset")
                                kept.shadow_x = offset.at(0) ?? 0.0
                                kept.shadow_y = offset.at(1) ?? 0.0
                                kept.shadow_blur = number_field(shadow, "blur") ?? 0.0
                                kept.shadow_colour = text_field(shadow, "colour")
                            }
                            if let shadow = object_field(style, "inner_shadow") {
                                let offset = numbers_at(shadow, "offset")
                                kept.inner_shadow_x = offset.at(0) ?? 0.0
                                kept.inner_shadow_y = offset.at(1) ?? 0.0
                                kept.inner_shadow_blur = number_field(shadow, "blur") ?? 0.0
                                kept.inner_shadow_colour = text_field(shadow, "colour")
                            }
                            let params = nested_params(row, "style", "params")
                            for key in params.keys() {
                                kept.params.put(key, params.get(key) ?? 0.0)
                            }
                            kept.stroke = nested_text(row, "style", "stroke")
                            kept.stroke_width = nested_number(row, "style", "stroke_width") ?? 0.0
                            kept.dash = numbers_at(nested_object(row, "style"), "dash")
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
                            matte_src: if let matte = object_field(area, "matte") {
                                text_field(matte, "src")
                            } else { "" },
                            matte_x: matte_at(area, 0),
                            matte_y: matte_at(area, 1),
                            matte_width: matte_at(area, 2),
                            matte_height: matte_at(area, 3),
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

/// How many stops the mark's gradient has.
fn gradient_stops(m: Mark) -> Int {
    (m.gradient_offsets ?? []).len()
}

/// Moves a stop of the mark's gradient along it, its offset 0 to 1.
action set_gradient_offset(m: Mark, at: Int, offset: Float) (
    label: "Move a gradient stop",
    did: "moved a gradient stop",
) {
    require m.gradient != "" "The mark is filled with no gradient"
    require offset >= 0.0 and offset <= 1.0 "A gradient stop is between 0 and 1"
    require at >= 0 and at < gradient_stops(m) "That stop is not one the gradient has"
    let offsets = m.gradient_offsets ?? []
    m.gradient_offsets = .some(offsets.map((o, i) -> if i == at { offset } else { o }))
}

/// Colours a stop of the mark's gradient.
action set_gradient_colour(m: Mark, at: Int, colour: Text) (
    label: "Colour a gradient stop",
    did: "coloured a gradient stop",
) {
    require m.gradient != "" "The mark is filled with no gradient"
    require at >= 0 and at < gradient_stops(m) "That stop is not one the gradient has"
    let colours = m.gradient_colours ?? []
    m.gradient_colours = .some(colours.map((c, i) -> if i == at { colour } else { c }))
}

/// Adds a stop to the mark's gradient, at `offset`, in `colour`.
action add_gradient_stop(m: Mark, offset: Float = 0.5, colour: Text = "#1d1b19") (
    icon: .plus,
    label: "Add a gradient stop",
    did: "added a gradient stop",
) {
    require m.gradient != "" "The mark is filled with no gradient"
    require offset >= 0.0 and offset <= 1.0 "A gradient stop is between 0 and 1"
    require gradient_stops(m) < 16 "A gradient has at most sixteen stops"
    m.gradient_offsets = .some((m.gradient_offsets ?? []) + [offset])
    m.gradient_colours = .some((m.gradient_colours ?? []) + [colour])
}

/// Takes a stop off the mark's gradient.
action remove_gradient_stop(m: Mark, at: Int) (
    icon: .minus,
    label: "Remove a gradient stop",
    did: "removed a gradient stop",
) {
    require m.gradient != "" "The mark is filled with no gradient"
    require at >= 0 and at < gradient_stops(m) "That stop is not one the gradient has"
    require gradient_stops(m) > 2 "A gradient keeps two stops at least"
    let offsets = m.gradient_offsets ?? []
    let colours = m.gradient_colours ?? []
    m.gradient_offsets = .some(
        offsets.map((o, i) -> if i == at { -1.0 } else { o }).filter(o -> o >= 0.0),
    )
    m.gradient_colours = .some(
        colours.map((c, i) -> if i == at { "" } else { c }).filter(c -> c != ""),
    )
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
            exposure: layer.exposure,
            contrast: layer.contrast,
            highlights: layer.highlights,
            shadows: layer.shadows,
            saturation: layer.saturation,
            vibrance: layer.vibrance,
            hue: layer.hue,
            temperature: layer.temperature,
            tint: layer.tint,
            adjust_grayscale: layer.adjust_grayscale,
            adjust_invert: layer.adjust_invert,
            blur: layer.blur,
            sharpen: layer.sharpen,
            vignette: layer.vignette,
            curve: layer.curve,
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
                transform: m.transform,
                clip: m.clip,
                pixelated: m.pixelated,
                grain: m.grain,
                feather: m.feather,
                shadow_x: m.shadow_x,
                shadow_y: m.shadow_y,
                shadow_blur: m.shadow_blur,
                shadow_colour: m.shadow_colour,
                inner_shadow_x: m.inner_shadow_x,
                inner_shadow_y: m.inner_shadow_y,
                inner_shadow_blur: m.inner_shadow_blur,
                inner_shadow_colour: m.inner_shadow_colour,
                curve: m.curve,
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

/// Draws a bar chart from labels and values, so a person or an agent gives the numbers and the
/// artwork gains bars, their labels and the axis under them as ordinary marks in one group: every
/// tool that works on marks works on the chart, and one undo takes it away. `kind` is `bar` or
/// `hbar`; `box` is the bars' area `[x, y, width, height]`, the artwork less margins when left
/// out, and `max` scales the bars to a value beyond their largest.
action add_chart(
    a: Artwork,
    labels: List<Text>,
    values: List<Float>,
    kind: Text = "bar",
    box: List<Float> = [],
    max: Float = 0.0,
    bar_width: Float = 0.0,
    colour: Text = "#4e79a7",
    label_size: Float = 0.0,
    values_shown: Bool = false,
    title: Text = "",
    layer: Option<Layer> = .none,
) -> Int (icon: .bar_chart, label: "Draw a chart", did: "drew a chart") {
    require values.len() > 0 "A chart is drawn from at least one value"
    require labels.len() == values.len() "Give one label per value"
    require values.len() <= 200 "A chart draws at most 200 bars"
    require values.all(v -> v >= 0.0) "A chart's values are zero or more"
    require kind == "bar" or kind == "hbar" "A chart is bars or a horizontal bar chart"
    require max >= 0.0 "A chart's scale is zero or more"
    require max == 0.0 or max >= (values.sort_by(v -> v, desc).first() ?? 0.0) "A chart's scale is at least its largest value"
    require bar_width >= 0.0 "A bar is zero or more wide"
    require label_size >= 0.0 "A chart's labels are zero or more large"
    let n = values.len()
    let horizontal = kind == "hbar"
    let largest = values.sort_by(v -> v, desc).first() ?? 0.0
    let scale = if max > 0.0 { max } else if largest > 0.0 { largest } else { 1.0 }
    let area = if box.len() >= 4 {
        box
    } else {
        chart_area(a, horizontal, title != "", values_shown)
    }
    let across = if horizontal { area.at(3) ?? 1.0 } else { area.at(2) ?? 1.0 }
    if across < 1.0 {
        a.notice = "A chart's area is one unit or more each way"
        return 0
    }
    if bar_width > 0.0 and bar_width * n.to_float() > across + 1e-6 {
        a.notice = "{n} bars {bar_width} wide do not fit in the area's {across}"
        return 0
    }
    let width = if bar_width > 0.0 {
        bar_width
    } else {
        across / (n.to_float() + (n - 1).to_float() * 2.0 / 3.0)
    }
    let pitch = if n > 1 {
        (across - width * n.to_float()) / (n - 1).to_float() + width
    } else { 0.0 }
    let first = if n > 1 { 0.0 } else { (across - width) / 2.0 }
    let size = if label_size > 0.0 {
        label_size
    } else {
        (a.width.min(a.height) / 25.0).clamp(10.0, 48.0).round(0)
    }
    let on = if let chosen = layer { chosen } else { top_layer(a) }
    let group = if a.marks.find(m -> m.group == "chart") == .none {
        "chart"
    } else { "chart-{a.marks.len()}" }
    let x = area.at(0) ?? 0.0
    let y = area.at(1) ?? 0.0
    let h = area.at(3) ?? 0.0
    let length = if horizontal { area.at(2) ?? 1.0 } else { h }
    let made: List<Mark> = []
    if title != "" {
        let title_size = (size * 1.4).round(0)
        let above = if values_shown and not horizontal { size * 1.4 } else { 0.0 }
        let t = add_text(
            a,
            .some(on),
            title,
            x + (area.at(2) ?? 0.0) / 2.0,
            y - above - title_size * 0.6,
            title_size,
            "#000000",
            "",
            700,
            false,
            "center",
            0.0,
            0.0,
            "",
        )
        made = made + [t]
    }
    // The axis just outside the area, so the bars cover none of it.
    let axis = if horizontal {
        add_shape(a, .some(on), rect_path([x - 1.0, y], [x, y + h]), "#666666")
    } else {
        add_shape(
            a,
            .some(on),
            rect_path([x, y + h], [x + (area.at(2) ?? 0.0), y + h + 2.0]),
            "#666666",
        )
    }
    made = made + [axis]
    for i in values.map((v, i) -> i) {
        let label = labels.at(i) ?? ""
        let value = values.at(i) ?? 0.0
        let bar_length = if value > 0.0 { (value / scale * length).max(1.0) } else { 0.0 }
        let offset = first + i.to_float() * pitch
        let bx = if horizontal { x } else { x + offset }
        let by = if horizontal { y + offset } else { y + h - bar_length }
        let bw = if horizontal { bar_length } else { width }
        let bh = if horizontal { width } else { bar_length }
        if value > 0.0 {
            let bar = add_shape(a, .some(on), rect_path([bx, by], [bx + bw, by + bh]), colour)
            bar.name = label
            made = made + [bar]
        }
        if label != "" {
            let at = if horizontal {
                [x - 2.0 - size * 0.4, by + bh / 2.0 + size * 0.35]
            } else { [bx + bw / 2.0, y + h + size * 1.2] }
            let t = add_text(
                a,
                .some(on),
                label,
                at.at(0) ?? 0.0,
                at.at(1) ?? 0.0,
                size,
                "#000000",
                "",
                0,
                false,
                if horizontal { "end" } else { "center" },
                0.0,
                0.0,
                "",
            )
            made = made + [t]
        }
        if values_shown {
            let at = if horizontal {
                [bx + bw + size * 0.4, by + bh / 2.0 + size * 0.35]
            } else { [bx + bw / 2.0, by - size * 0.35] }
            let t = add_text(
                a,
                .some(on),
                chart_number(value),
                at.at(0) ?? 0.0,
                at.at(1) ?? 0.0,
                size,
                "#000000",
                "",
                0,
                false,
                if horizontal { "start" } else { "center" },
                0.0,
                0.0,
                "",
            )
            made = made + [t]
        }
    }
    for m in made {
        m.group = group
    }
    made.len()
}

/// The area a chart stands in when none is given: the artwork less margins for its labels, its
/// title and the values.
fn chart_area(a: Artwork, horizontal: Bool, titled: Bool, values_shown: Bool) -> List<Float> {
    let width = a.width
    let height = a.height
    let top = height * 0.08 + if titled { height * 0.08 } else { 0.0 } + if values_shown and not horizontal { height * 0.06 } else { 0.0 }
    let left = if horizontal { width * 0.22 } else { width * 0.08 }
    let right = if horizontal {
        if values_shown { width * 0.12 } else { width * 0.06 }
    } else { width * 0.08 }
    let bottom = if horizontal { height * 0.06 } else { height * 0.14 }
    [left, top, width - left - right, height - top - bottom]
}

/// A value as a chart writes it above its bar: whole numbers without a point, others to at most
/// two decimals.
fn chart_number(value: Float) -> Text {
    if value.round(0) == value { value.round(0).to_text() } else { value.round(2).to_text() }
}

/// Outline dashes a mark's outline in: `[]` restores a solid line, `[8, 4]` is dashed and
/// `[1, 3]` dotted.
action set_dash(m: Mark, dash: List<Float>) (
    icon: .minus,
    label: "Dash the outline",
    did: "dashed an outline",
) {
    require dash.len() <= 16 "An outline takes at most sixteen dash lengths"
    require dash.all(length -> length >= 0.0) "A dash length is zero or more"
    require dash.is_empty() or dash.sum() > 0.0 "Dashes are not all zero"
    m.dash = dash
}

/// Outlines a mark: its colour and how wide it is, drawn outside its edge, so a shape stands out
/// from what is under it.
action set_outline(m: Mark, stroke: Text = m.stroke, width: Float = m.stroke_width) (
    icon: .square,
    label: "Outline the mark",
    did: "outlined a mark",
) {
    require width >= 0.0 and width <= 256.0 "An outline is 0 to 256 units wide"
    m.stroke = stroke
    m.stroke_width = if stroke == "" { 0.0 } else { width }
}

/// Picks the colour the artwork shows at a point, as the eye sees it: every layer composited,
/// erasers taken off, the reference over it. The window loads it on the brush.
action sample_colour(a: Artwork, at: List<Float>) -> Option<Text> uses image (
    icon: .pipette,
    label: "Pick the colour",
) {
    require at.len() >= 2 "A point is x and y"
    let request = "\{\"at\":["
        + (at.at(0) ?? 0.0).to_text()
        + ","
        + (at.at(1) ?? 0.0).to_text()
        + "]\}"
    match canvas.sample(artwork_json(a), request, picture_addresses(a)) {
        .ok(colour) => {
            a.notice = ""
            return .some(colour)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// The colour the artwork shows at a point, as `#rrggbb`, for the window to load on the brush:
/// what the eyedropper reads, and nothing when the device cannot draw.
fn colour_at(a: Artwork, at: List<Float>) -> Option<Text> {
    let request = "\{\"at\":["
        + (at.at(0) ?? 0.0).to_text()
        + ","
        + (at.at(1) ?? 0.0).to_text()
        + "]\}"
    match canvas.sample(artwork_json(a), request, picture_addresses(a)) {
        .ok(colour) => .some(colour),
        .err(_) => .none,
    }
}

/// Moves the picked marks by `dx`, `dy` units, as the arrow keys nudge them.
action nudge_picked(a: Artwork, dx: Float, dy: Float) (label: "Nudge the picked marks") {
    require a.picked.len() > 0 "Pick the marks to nudge"
    for m in a.picked {
        move_mark(m, dx, dy)
    }
}

/// Removes the marks a person picked.
destructive action delete_picked(a: Artwork) (icon: .trash, label: "Delete the picked marks") {
    require a.picked.len() > 0 "Pick the marks to delete"
    for m in a.picked.map(m -> m) {
        a.marks.remove(m)
    }
    a.picked = []
}

/// Aligns marks on an edge of their own extent, the artwork's, or another mark's, and spaces them
/// evenly along an axis. A mark can be aligned and spaced in one go. Answers how many moved.
action align_marks(
    a: Artwork,
    marks: List<Mark>,
    edge: Text = "",
    distribute: Text = "",
    to_artwork: Bool = false,
    to: Option<Mark> = .none,
) -> Int (icon: .align_start_horizontal, label: "Align marks", did: "aligned marks") {
    require marks.len() > 0 or a.picked.len() > 0 "Name the marks to align, or pick them"
    require edge == "left"
        or edge == "center"
        or edge == "right"
        or edge == "top"
        or edge == "middle"
        or edge == "bottom"
        or edge == "" "An edge is left, center, right, top, middle or bottom"
    require distribute == "" or distribute == "horizontal" or distribute == "vertical" "Marks spread across or down"
    require edge != "" or distribute != "" "Give an edge to line up on or an axis to spread along"
    require distribute == "" or marks.len() >= 2 or a.picked.len() >= 2 or to_artwork or to != .none "Spreading needs two or more marks, or a frame to spread across"
    let chosen = if marks.len() == 0 { a.picked } else { marks }
    let held = match canvas.inspect(artwork_json(a), json.encode(InspectAsk(limit: .some(400)))) {
        .ok(answer) => match json.parse(answer) {
            .ok(value) => array_field(value, "marks"),
            .err(_) => [],
        },
        .err(_) => [],
    }
    if held.len() == 0 {
        a.notice = "The artwork cannot be read to align it"
        return 0
    }
    let boxes = chosen.map(m -> bounds_of(held, mark_id(a, m)))
    if not boxes.all(b -> b.len() >= 4) {
        a.notice = "Some marks cannot be measured"
        return 0
    }
    let target = if to_artwork {
        [0.0, 0.0, a.width, a.height]
    } else if let other = to { bounds_of(held, mark_id(a, other)) } else { extent_of(boxes) }
    if target.len() < 4 {
        a.notice = "There is nothing to align to"
        return 0
    }
    let horizontal = distribute == "horizontal"
    let frame = if to_artwork or to != .none { target } else { extent_of(boxes) }
    let order = chosen.map((m, i) -> i).sort_by(i -> box_centre(boxes.at(i) ?? [], horizontal))
    let shifts = chosen.map((m, i) -> shift_for(boxes, order, i, edge, distribute, target, frame))
    for i in chosen.map((m, i) -> i) {
        let shift = shifts.at(i) ?? [0.0, 0.0]
        let dx = shift.at(0) ?? 0.0
        let dy = shift.at(1) ?? 0.0
        if dx != 0.0 or dy != 0.0 {
            if let m = chosen.at(i) { move_mark(m, dx, dy) }
        }
    }
    chosen.len()
}

/// The id the device's drawing engine names a mark by, in the order the artwork keeps its marks.
fn mark_id(a: Artwork, m: Mark) -> Text {
    "mark-{a.marks.index_of(m) ?? 0}"
}

/// The bounds `[x, y, width, height]` an inspection gives a mark, or nothing when it does not
/// list it.
fn bounds_of(held: List<json.Value>, id: Text) -> List<Float> {
    if let row = held.find(r -> text_field(r, "id") == id) { numbers_at(row, "bounds") } else { [] }
}

/// The box containing every box: `[x0, y0, x1, y1]`.
fn extent_of(boxes: List<List<Float>>) -> List<Float> {
    let left = boxes.map(b -> b.at(0) ?? 0.0).sort_by(v -> v).first() ?? 0.0
    let top = boxes.map(b -> b.at(1) ?? 0.0).sort_by(v -> v).first() ?? 0.0
    let right = boxes.map(b -> (b.at(0) ?? 0.0) + (b.at(2) ?? 0.0)).sort_by(v -> v, desc).first()
        ?? 0.0
    let bottom = boxes.map(b -> (b.at(1) ?? 0.0) + (b.at(3) ?? 0.0)).sort_by(v -> v, desc).first()
        ?? 0.0
    [left, top, right, bottom]
}

/// A box's middle along an axis.
fn box_centre(box: List<Float>, horizontal: Bool) -> Float {
    if horizontal {
        (box.at(0) ?? 0.0) + (box.at(2) ?? 0.0) / 2.0
    } else {
        (box.at(1) ?? 0.0) + (box.at(3) ?? 0.0) / 2.0
    }
}

/// How far the `index`th box moves: onto the edge it is aligned by, plus its share of spreading
/// the boxes along an axis inside `frame`.
fn shift_for(
    boxes: List<List<Float>>,
    order: List<Int>,
    index: Int,
    edge: Text,
    distribute: Text,
    target: List<Float>,
    frame: List<Float>,
) -> List<Float> {
    let b = boxes.at(index) ?? [0.0, 0.0, 0.0, 0.0]
    let horizontal = distribute == "horizontal"
    let along = if distribute == "" {
        0.0
    } else {
        spread_shift(boxes, order, index, horizontal, frame)
    }
    [edge_shift(b, target, edge, true) + along, edge_shift(b, target, edge, false)]
}

/// How far a box moves to sit on `edge` of the target box `[x, y, width, height]`, across the x
/// axis (`along` true) or the y axis.
fn edge_shift(box: List<Float>, target: List<Float>, edge: Text, along: Bool) -> Float {
    if along {
        let left = (target.at(0) ?? 0.0) - (box.at(0) ?? 0.0)
        let centre = (target.at(0) ?? 0.0) + (target.at(2) ?? 0.0) / 2.0
        -((box.at(0) ?? 0.0) + (box.at(2) ?? 0.0) / 2.0)
        let right = (target.at(0) ?? 0.0) + (target.at(2) ?? 0.0)
        -((box.at(0) ?? 0.0) + (box.at(2) ?? 0.0))
        if edge == "left" {
            left
        } else if edge == "center" { centre } else if edge == "right" { right } else { 0.0 }
    } else {
        let top = (target.at(1) ?? 0.0) - (box.at(1) ?? 0.0)
        let middle = (target.at(1) ?? 0.0) + (target.at(3) ?? 0.0) / 2.0
        -((box.at(1) ?? 0.0) + (box.at(3) ?? 0.0) / 2.0)
        let bottom = (target.at(1) ?? 0.0) + (target.at(3) ?? 0.0)
        -((box.at(1) ?? 0.0) + (box.at(3) ?? 0.0))
        if edge == "top" {
            top
        } else if edge == "middle" { middle } else if edge == "bottom" { bottom } else { 0.0 }
    }
}

/// How far the `index`th box moves so the boxes are spaced evenly along an axis inside `frame`,
/// with equal gaps between them and none outside.
fn spread_shift(
    boxes: List<List<Float>>,
    order: List<Int>,
    index: Int,
    horizontal: Bool,
    frame: List<Float>,
) -> Float {
    let from = if horizontal { frame.at(0) ?? 0.0 } else { frame.at(1) ?? 0.0 }
    let to = from + if horizontal { frame.at(2) ?? 0.0 } else { frame.at(3) ?? 0.0 }
    let total = order.map(i -> box_length(boxes, i, horizontal)).sum()
    let gaps = (order.len() - 1).max(1)
    let gap = (to - from - total) / gaps.to_float()
    let at = order.index_of(index) ?? 0
    let before = order.take(at).map(i -> box_length(boxes, i, horizontal)).sum()
    let cursor = if order.len() == 1 {
        from + (to - from - total) / 2.0
    } else { from + before + gap * at.to_float() }
    cursor - box_start(boxes, index, horizontal)
}

/// A box's length along an axis.
fn box_length(boxes: List<List<Float>>, i: Int, horizontal: Bool) -> Float {
    let b = boxes.at(i) ?? []
    if horizontal { b.at(2) ?? 0.0 } else { b.at(3) ?? 0.0 }
}

/// A box's start along an axis.
fn box_start(boxes: List<List<Float>>, i: Int, horizontal: Bool) -> Float {
    let b = boxes.at(i) ?? []
    if horizontal { b.at(0) ?? 0.0 } else { b.at(1) ?? 0.0 }
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
        .ok(answer) => {
            a.selection = selection_of(answer)
            return .some(answer)
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
        .ok(answer) => {
            a.selection = selection_of(answer)
            return .some(answer)
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

/// What a selection asks the device's drawing engine for: a shape, a lasso, marks, and how it
/// meets the area there is. What the engine's own `select` takes.
type SelectAsk {
    rect: Option<List<Float>> = .none
    ellipse: Option<List<Float>> = .none
    points: Option<List<List<Float>>> = .none
    d: Option<Text> = .none
    marks: Option<List<Text>> = .none
    mode: Option<Text> = .none
    feather: Option<Float> = .none
    grow: Option<Float> = .none
    invert: Option<Bool> = .none
}

/// What inspecting the artwork asks for: its layers and newest marks.
type InspectAsk {
    palette: Option<Bool> = .none
    region: Option<List<Float>> = .none
    layer: Option<Text> = .none
    limit: Option<Int> = .none
}

/// What removing marks along a path asks for.
type RemoveAsk {
    along: List<List<Float>>
    layer: Option<Text> = .none
}

/// What comparing two versions asks for.
type DiffAsk {
    from: Text
    to: Option<Text> = .none
    look: Option<Bool> = .none
    overlay: Option<Bool> = .none
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
    require marks.len() >= 2 or marks.len() == 0 and a.picked.len() >= 2 "Name two or more marks to combine, or pick them"
    require op == "union" or op == "subtract" or op == "intersect" or op == "exclude" "Shapes unite, subtract, intersect or exclude"
    let chosen = if marks.len() == 0 { a.picked } else { marks }
    let ids = chosen.map(m -> "mark-{a.marks.index_of(m) ?? 0}")
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
                        .filter(m -> chosen.find(n -> n == m) != .none)
                        .first() {
                    found
                } else if let named = chosen.first() {
                    named
                } else {
                    a.marks.insert(Mark(layer: top_layer(a)))
                }
                let fill = text_field(held, "fill")
                for m in chosen {
                    if m != kept { a.marks.remove(m) }
                }
                a.picked = []
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
    require marks.len() > 0 or a.picked.len() > 0 "Name the marks to measure, or pick them"
    let chosen = if marks.len() == 0 { a.picked } else { marks }
    let ids = chosen.map(m -> "mark-{a.marks.index_of(m) ?? 0}")
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

/// Compares the artwork with the version history names, overlaid on the surface: what changed
/// between the two, and the changed region as it now stands where it stands.
action compare_versions(a: Artwork, change: Text) -> Option<Text> uses image (
    icon: .history,
    label: "Compare versions",
    did: "compared versions",
) {
    let before = record_at(a, change)
    match past_artwork_json(before) {
        .some(older) => {
            let after = artwork_json(a)
            let ask = json.encode(DiffAsk(from: change, look: .none, overlay: .none))
            match canvas.diff(older, after, ask, picture_addresses(a)) {
                .ok(info) => {
                    a.version_note = diff_said(info)
                    a.version_region = changed_region(info)
                    let with_picture = json.encode(
                        DiffAsk(from: change, look: .some(true), overlay: .some(true)),
                    )
                    match canvas.diff_picture(older, after, with_picture, picture_addresses(a)) {
                        .ok(file) => {
                            a.version_overlay = file.url
                            a.notice = ""
                        }
                        .err(why) => {
                            a.notice = why
                        }
                    }
                    return .some(info)
                }
                .err(why) => {
                    a.notice = why
                    return .none
                }
            }
        }
        .none => {
            a.notice = "That version of the artwork cannot be read"
            return .none
        }
    }
}

/// Whether a version of the artwork only showed it (a look drawn, a comparison laid over it)
/// rather than changed its work: the timeline leaves those out.
fn shown_only(kind: Text, fields: List<Text>) -> Bool {
    kind == "changed"
        and fields.len() > 0
        and fields.all(
            f -> f == "look"
                or f == "comparison"
                or f == "notice"
                or f == "version_note"
                or f == "version_overlay"
                or f == "version_region",
        )
}

/// The places `0.0` to `n - 1` in order: one stop on a timeline for each of `n` versions.
fn places(n: Int) -> List<Float> {
    if n <= 0 {
        []
    } else if n == 1 {
        [0.0]
    } else {
        let half = n / 2
        places(half) + places(n - half).map(x -> x + half.to_float())
    }
}

/// Compares the artwork with its version at `place` on the timeline, the oldest at `0.0`. At
/// the newest the comparison is taken away, as the artwork stands as it is.
action compare_at(a: Artwork, place: Float) -> Option<Text> uses image (
    icon: .history,
    label: "Compare with a version",
    did: "compared versions",
) {
    let versions = history(a).filter(v -> not shown_only(v.kind, v.fields)).sort_by(v -> v.at)
    let at = place.round()
    if at < versions.len() - 1 {
        if let v = versions.at(at) {
            return compare_versions(a, change_of(v))
        }
    }
    clear_comparison(a)
    return .none
}

/// The artwork's versions along a track, oldest at the left: dragging the knob along it
/// compares the artwork with the version it reaches, and what changed since is laid over the
/// surface. The knob rests on the newest, the artwork as it stands. The track is read when the
/// person asks for it: an artwork of ten thousand marks has as many versions, and opening it
/// reads none of them.
view version_timeline(a: Artwork) {
    state shown: Bool = false
    state at: Float = -1.0
    if not shown {
        row(gap: .related, align: .center) {
            ui.Button("Versions", icon: .history) {
                on press: set(shown, true)
            }
        }
    } else {
        let versions = history(a).filter(v -> not shown_only(v.kind, v.fields)).sort_by(v -> v.at)
        let newest = (versions.len() - 1).max(0).to_float()
        let on = if at < 0.0 or at > newest { newest } else { at }
        row(gap: .related, align: .center) {
            ui.Text("Versions", role: .caption, secondary: true)
            ui.Slider(
                value: on,
                label: "Versions of {a.title}",
                min: 0.0,
                max: newest,
                step: 1.0,
                stops: places(versions.len()),
                fill,
            ) {
                on change(t): set(at, t).then(compare_at(a, t))
            }
            if let seen = versions.at(on.round()) {
                ui.Text("{seen.what} · {seen.who} · {seen.at}", role: .caption, secondary: true)
            }
            ui.Button("Back to now", icon: .x) {
                on press: set(at, -1.0).then(set(shown, false)).then(clear_comparison(a))
            }
        }
        if a.version_note != "" {
            ui.Text(a.version_note, role: .caption, secondary: true)
        }
    }
}

/// Takes the version comparison away from the surface.
action clear_comparison(a: Artwork) (label: "Clear the comparison", did: "cleared a comparison") {
    a.version_overlay = ""
    a.version_note = ""
    a.version_region = []
}

/// What comparing two versions found, in a line a person reads.
fn diff_said(info: Text) -> Text {
    match json.parse(info) {
        .ok(held) => {
            if flag_field(held, "same") ?? false {
                return "Those versions are the same"
            }
            let marks = array_field(held, "marks").len()
            let layers = array_field(held, "layers").len()
            let media = array_field(held, "media").len()
            let artwork = array_field(held, "artwork").len()
            "Changed {marks} marks, {layers} layers, {media} media and {artwork} artwork fields"
        }
        .err(_) => "Compared versions",
    }
}

/// The region comparing two versions changed, `[x, y, width, height]`.
fn changed_region(info: Text) -> List<Float> {
    match json.parse(info) {
        .ok(held) => numbers_at(held, "changed_region"),
        .err(_) => [],
    }
}

/// A version's record as the engine's artwork JSON: each field by name, its collection items
/// inline with their own fields. The store read the state; the artwork's shape is the app's.
fn past_artwork_json(record: Text) -> Option<Text> {
    match json.parse(record) {
        .ok(held) => {
            let fields = object_field(held, "fields") ?? held
            let layers = array_field(fields, "layers")
            let layer_names: Map<Text> = Map<Text>()
            for row in layers {
                layer_names.put(text_field(row, "id"), layer_name_of(row))
            }
            json.encode(
                ArtworkJson(
                    title: text_field(fields, "title"),
                    width: number_field(fields, "width") ?? 1024.0,
                    height: number_field(fields, "height") ?? 768.0,
                    ground_texture: text_field(fields, "ground_texture"),
                    ground_colour: text_field(fields, "ground_colour"),
                    layers: layers.map(row -> layer_json_of(row)),
                    marks: array_field(fields, "marks").map(row -> mark_json_of(row, layer_names)),
                    media: array_field(fields, "media").map(row -> medium_json_of(row)),
                    selection: area_json_of(fields),
                    reference: reference_json_of(fields),
                ),
            )
        }
        .err(_) => .none,
    }
}

fn past_fields(row: json.Value) -> json.Value {
    object_field(row, "fields") ?? row
}

fn layer_name_of(row: json.Value) -> Text {
    text_field(past_fields(row), "name")
}

fn layer_json_of(row: json.Value) -> LayerJson {
    let f = past_fields(row)
    LayerJson(
        name: text_field(f, "name"),
        medium: text_field(f, "medium"),
        opacity: number_field(f, "opacity") ?? 1.0,
        visible: flag_field(f, "visible") ?? true,
        locked: flag_field(f, "locked") ?? false,
        texture: number_field(f, "texture") ?? 0.0,
        group: text_field(f, "group"),
        mask_of: text_field(f, "mask_of"),
        base: number_field(f, "base") ?? 1.0,
        invert: flag_field(f, "invert") ?? false,
        clip: flag_field(f, "clip") ?? false,
        unlinked: flag_field(f, "unlinked") ?? false,
        exposure: number_field(f, "exposure") ?? 0.0,
        contrast: number_field(f, "contrast") ?? 0.0,
        highlights: number_field(f, "highlights") ?? 0.0,
        shadows: number_field(f, "shadows") ?? 0.0,
        saturation: number_field(f, "saturation") ?? 0.0,
        vibrance: number_field(f, "vibrance") ?? 0.0,
        hue: number_field(f, "hue") ?? 0.0,
        temperature: number_field(f, "temperature") ?? 0.0,
        tint: number_field(f, "tint") ?? 0.0,
        grayscale: flag_field(f, "adjust_grayscale") ?? false,
        invert_adjust: flag_field(f, "adjust_invert") ?? false,
        blur: number_field(f, "blur") ?? 0.0,
        sharpen: number_field(f, "sharpen") ?? 0.0,
        vignette: number_field(f, "vignette") ?? 0.0,
        curve: curves_of(f),
    )
}

fn mark_json_of(row: json.Value, layer_names: Map<Text>) -> MarkJson {
    let f = past_fields(row)
    let layer_ref = object_field(f, "layer") ?? f
    let layer = LayerJson(
        name: layer_names.get(text_field(layer_ref, "ref")) ?? "",
        medium: "",
        opacity: 1.0,
        visible: true,
        locked: false,
        texture: 0.0,
        group: "",
        mask_of: "",
        base: 1.0,
        invert: false,
        clip: false,
        unlinked: false,
    )
    MarkJson(
        name: text_field(f, "name"),
        layer: layer,
        kind: text_field(f, "kind"),
        group: text_field(f, "group"),
        points: numbers_at(f, "points"),
        d: text_field(f, "d"),
        size: number_field(f, "size") ?? 6.0,
        colour: text_field(f, "colour"),
        brush: text_field(f, "brush"),
        opacity: number_field(f, "opacity") ?? 1.0,
        src: text_field(f, "src"),
        x: number_field(f, "x") ?? 0.0,
        y: number_field(f, "y") ?? 0.0,
        width: number_field(f, "width") ?? 0.0,
        height: number_field(f, "height") ?? 0.0,
        blend: text_field(f, "blend"),
        crop_x: number_field(f, "crop_x") ?? 0.0,
        crop_y: number_field(f, "crop_y") ?? 0.0,
        crop_width: number_field(f, "crop_width") ?? 1.0,
        crop_height: number_field(f, "crop_height") ?? 1.0,
        straighten: number_field(f, "straighten") ?? 0.0,
        flip_h: flag_field(f, "flip_h") ?? false,
        flip_v: flag_field(f, "flip_v") ?? false,
        exposure: number_field(f, "exposure") ?? 0.0,
        contrast: number_field(f, "contrast") ?? 0.0,
        highlights: number_field(f, "highlights") ?? 0.0,
        shadows: number_field(f, "shadows") ?? 0.0,
        saturation: number_field(f, "saturation") ?? 0.0,
        vibrance: number_field(f, "vibrance") ?? 0.0,
        hue: number_field(f, "hue") ?? 0.0,
        temperature: number_field(f, "temperature") ?? 0.0,
        tint: number_field(f, "tint") ?? 0.0,
        grayscale: flag_field(f, "grayscale") ?? false,
        invert: flag_field(f, "invert") ?? false,
        blur: number_field(f, "blur") ?? 0.0,
        sharpen: number_field(f, "sharpen") ?? 0.0,
        vignette: number_field(f, "vignette") ?? 0.0,
        gradient_kind: text_field(f, "gradient"),
        gradient_x0: number_field(f, "gradient_x0") ?? 0.0,
        gradient_y0: number_field(f, "gradient_y0") ?? 0.0,
        gradient_x1: number_field(f, "gradient_x1") ?? 0.0,
        gradient_y1: number_field(f, "gradient_y1") ?? 0.0,
        gradient_offsets: numbers_at(f, "gradient_offsets"),
        gradient_colours: texts_at(f, "gradient_colours"),
        text: text_field(f, "text"),
        font: text_field(f, "font"),
        weight: int_field(f, "weight"),
        italic: flag_field(f, "italic") ?? false,
        align: text_field(f, "align"),
        line_height: number_field(f, "line_height") ?? 0.0,
        tracking: number_field(f, "tracking") ?? 0.0,
        text_path: text_field(f, "text_path"),
        wet: flag_field(f, "wet") ?? false,
        params: params_of(f),
        transform: numbers_at(f, "transform"),
        clip: text_field(f, "clip"),
        pixelated: flag_field(f, "pixelated") ?? false,
        grain: number_field(f, "grain") ?? 0.0,
        feather: number_field(f, "feather") ?? 0.0,
        shadow_x: number_field(f, "shadow_x") ?? 0.0,
        shadow_y: number_field(f, "shadow_y") ?? 0.0,
        shadow_blur: number_field(f, "shadow_blur") ?? 0.0,
        shadow_colour: text_field(f, "shadow_colour"),
        inner_shadow_x: number_field(f, "inner_shadow_x") ?? 0.0,
        inner_shadow_y: number_field(f, "inner_shadow_y") ?? 0.0,
        inner_shadow_blur: number_field(f, "inner_shadow_blur") ?? 0.0,
        inner_shadow_colour: text_field(f, "inner_shadow_colour"),
        curve: curves_of(f),
    )
}

fn medium_json_of(row: json.Value) -> MediumJson {
    let f = past_fields(row)
    MediumJson(
        id: text_field(f, "key"),
        label: text_field(f, "label"),
        engine: text_field(f, "engine"),
        description: text_field(f, "description"),
        ground: text_field(f, "ground"),
        params: params_of(f),
        palette: array_field(f, "palette").map(p -> pigment_json_of(p)),
    )
}

fn pigment_json_of(row: json.Value) -> PigmentJson {
    let held = past_fields(row)
    PigmentJson(
        name: text_field(held, "name"),
        colour: text_field(held, "colour"),
        granulation: number_field(held, "granulation") ?? 0.0,
        strength: number_field(held, "strength") ?? 1.0,
    )
}

fn area_json_of(fields: json.Value) -> Option<AreaJson> {
    if let held = object_field(fields, "selection") {
        if as_object(held) != .none {
            return .some(
                AreaJson(
                    d: text_field(held, "d"),
                    feather: number_field(held, "feather") ?? 0.0,
                    matte: if let matte = object_field(held, "matte") {
                        .some(
                            MatteJson(
                                src: text_field(matte, "src"),
                                rect: numbers_at(matte, "rect"),
                            ),
                        )
                    } else { .none },
                ),
            )
        }
    }
    .none
}

fn reference_json_of(fields: json.Value) -> Option<ReferenceJson> {
    if let held = object_field(fields, "reference") {
        if as_object(held) != .none {
            let width = number_field(held, "width") ?? 0.0
            let height = number_field(held, "height") ?? 0.0
            return .some(
                ReferenceJson(
                    image: text_field(held, "image"),
                    frame: if width > 0.0 and height > 0.0 {
                        .some(
                            [
                                number_field(held, "x") ?? 0.0,
                                number_field(held, "y") ?? 0.0,
                                width,
                                height,
                            ],
                        )
                    } else { .none },
                    opacity: number_field(held, "opacity") ?? 1.0,
                    visible: flag_field(held, "visible") ?? true,
                ),
            )
        }
    }
    .none
}

fn numbers_at(value: json.Value, key: Text) -> List<Float> {
    if let held = object_field(value, key) {
        (as_array(held) ?? []).map(item -> as_number(item) ?? 0.0)
    } else { [] }
}

fn texts_at(value: json.Value, key: Text) -> List<Text> {
    if let held = object_field(value, key) {
        (as_array(held) ?? []).map(item -> as_text(item) ?? "")
    } else { [] }
}

/// The `at`th number of the matte a selection keeps, when it keeps one.
fn matte_at(area: json.Value, at: Int) -> Float {
    if let matte = object_field(area, "matte") { number_at(matte, "rect", at) } else { 0.0 }
}

/// The JSON object `key` of the object `of` holds, else `of` itself.
fn nested_object(value: json.Value, key: Text) -> json.Value {
    object_field(value, key) ?? value
}

/// The tone curve a record keeps, read from JSON.
fn curves_of(value: json.Value) -> List<List<Float>> {
    array_field(value, "curve").map(pair -> curve_point_of(pair))
}

fn curve_point_of(pair: json.Value) -> List<Float> {
    match as_array(pair) {
        .some(items) => items.map(item -> as_number(item) ?? 0.0),
        .none => [],
    }
}

/// Moves a mark by `dx`, `dy` artwork units.
action move_mark(m: Mark, dx: Float, dy: Float) (
    icon: .move,
    label: "Move the mark",
    did: "moved a mark",
) {
    m.transform = composed(m.transform, [1.0, 0.0, 0.0, 1.0, dx, dy])
}

/// Scales a mark about `cx`, `cy` by `factor`.
action scale_mark(m: Mark, cx: Float, cy: Float, factor: Float) (
    icon: .maximize,
    label: "Scale the mark",
    did: "scaled a mark",
) {
    require factor > 0.0 "A scale is larger than nothing"
    m.transform = composed(
        m.transform,
        [factor, 0.0, 0.0, factor, cx - factor * cx, cy - factor * cy],
    )
}

/// Turns a mark about `cx`, `cy` by `degrees`.
action rotate_mark(m: Mark, cx: Float, cy: Float, degrees: Float) (
    icon: .rotate_cw,
    label: "Turn the mark",
    did: "turned a mark",
) {
    let radians = degrees * 3.141592653589793 / 180.0
    let cos = radians.cos()
    let sin = radians.sin()
    m.transform = composed(
        m.transform,
        [cos, sin, -sin, cos, cx - cos * cx + sin * cy, cy - sin * cx - cos * cy],
    )
}

/// `next` after `current`: one affine `[a, b, c, d, e, f]`, identity when empty.
fn composed(current: List<Float>, next: List<Float>) -> List<Float> {
    if current.len() < 6 {
        return next
    }
    let a1 = current.at(0) ?? 1.0
    let b1 = current.at(1) ?? 0.0
    let c1 = current.at(2) ?? 0.0
    let d1 = current.at(3) ?? 1.0
    let e1 = current.at(4) ?? 0.0
    let f1 = current.at(5) ?? 0.0
    let a2 = next.at(0) ?? 1.0
    let b2 = next.at(1) ?? 0.0
    let c2 = next.at(2) ?? 0.0
    let d2 = next.at(3) ?? 1.0
    let e2 = next.at(4) ?? 0.0
    let f2 = next.at(5) ?? 0.0
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

/// Cuts a mark to the shape of another mark or a group, or lets it stand whole.
action clip_mark(m: Mark, by: Text) (
    icon: .scissors,
    label: "Clip the mark",
    did: "clipped a mark",
) {
    m.clip = by.trim()
}

/// Sets how a mark is finished: nearest-neighbour pixels, grain, a feathered edge, and its
/// shadows. A parameter left out stays as it is.
action finish_mark(
    m: Mark,
    pixelated: Bool = m.pixelated,
    grain: Float = m.grain,
    feather: Float = m.feather,
    shadow_x: Float = m.shadow_x,
    shadow_y: Float = m.shadow_y,
    shadow_blur: Float = m.shadow_blur,
    shadow_colour: Text = m.shadow_colour,
    inner_shadow_x: Float = m.inner_shadow_x,
    inner_shadow_y: Float = m.inner_shadow_y,
    inner_shadow_blur: Float = m.inner_shadow_blur,
    inner_shadow_colour: Text = m.inner_shadow_colour,
) (icon: .sparkles, label: "Finish the mark", did: "finished a mark") {
    require grain >= 0.0 and grain <= 1.0 "Grain is 0 to 1"
    require feather >= 0.0 and feather <= 200.0 "A feather is 0 to 200 units"
    m.pixelated = pixelated
    m.grain = grain
    m.feather = feather
    m.shadow_x = shadow_x
    m.shadow_y = shadow_y
    m.shadow_blur = shadow_blur
    m.shadow_colour = shadow_colour
    m.inner_shadow_x = inner_shadow_x
    m.inner_shadow_y = inner_shadow_y
    m.inner_shadow_blur = inner_shadow_blur
    m.inner_shadow_colour = inner_shadow_colour
}

/// Sets a mark's tone curve, `[[in, out], …]` from 0 to 1.
action set_curve(m: Mark, curve: List<List<Float>>) (
    label: "Set the tone curve",
    did: "set a tone curve",
) {
    require curve.all(point -> point.len() >= 2) "A curve point is in and out"
    m.curve = curve
}

/// Adjusts everything on a layer, after each mark's own adjustments.
action adjust_layer(
    layer: Layer,
    exposure: Float = layer.exposure,
    contrast: Float = layer.contrast,
    highlights: Float = layer.highlights,
    shadows: Float = layer.shadows,
    saturation: Float = layer.saturation,
    vibrance: Float = layer.vibrance,
    hue: Float = layer.hue,
    temperature: Float = layer.temperature,
    tint: Float = layer.tint,
    grayscale: Bool = layer.adjust_grayscale,
    invert: Bool = layer.adjust_invert,
    blur: Float = layer.blur,
    sharpen: Float = layer.sharpen,
    vignette: Float = layer.vignette,
) (icon: .sliders_horizontal, label: "Adjust the layer", did: "adjusted a layer") {
    layer.exposure = exposure
    layer.contrast = contrast
    layer.highlights = highlights
    layer.shadows = shadows
    layer.saturation = saturation
    layer.vibrance = vibrance
    layer.hue = hue
    layer.temperature = temperature
    layer.tint = tint
    layer.adjust_grayscale = grayscale
    layer.adjust_invert = invert
    layer.blur = blur
    layer.sharpen = sharpen
    layer.vignette = vignette
}

/// What the engine answered about a selection, as the artwork's selection.
fn selection_of(answer: Text) -> Option<Area> {
    match json.parse(answer) {
        .ok(held) => {
            let matte = numbers_at(held, "matte_rect")
            let area = Area(
                d: text_field(held, "d"),
                feather: number_field(held, "feather") ?? 0.0,
                matte_src: text_field(held, "matte_src"),
                matte_x: matte.at(0) ?? 0.0,
                matte_y: matte.at(1) ?? 0.0,
                matte_width: matte.at(2) ?? 0.0,
                matte_height: matte.at(3) ?? 0.0,
            )
            let kept: Option<Area> = .some(area)
            kept
        }
        .err(_) => .none,
    }
}

/// Asks the engine for a selection, and keeps the area it answers.
action ask_selection(a: Artwork, ask: Text) -> Option<Text> uses image {
    match canvas.select(artwork_json(a), ask) {
        .ok(answer) => {
            a.selection = selection_of(answer)
            return .some(answer)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

/// Selects a rectangle or an ellipse from one corner of a drag to the other, meeting the area
/// there is by `mode` (`replace`, `add`, `subtract` or `intersect`).
action select_shape(
    a: Artwork,
    kind: Text,
    from: List<Float>,
    to: List<Float>,
    mode: Text = "replace",
    feather: Float = 0.0,
    grow: Float = 0.0,
    invert: Bool = false,
) -> Option<Text> uses image (icon: .circle_dashed, label: "Select a shape") {
    require kind == "rect" or kind == "ellipse" "A selection shape is a rectangle or an ellipse"
    require from.len() >= 2 and to.len() >= 2 "A selection drag has two corners"
    require mode == "replace" or mode == "add" or mode == "subtract" or mode == "intersect" "A selection meets the one there is by replace, add, subtract or intersect"
    let x0 = from.at(0) ?? 0.0
    let y0 = from.at(1) ?? 0.0
    let x1 = to.at(0) ?? 0.0
    let y1 = to.at(1) ?? 0.0
    let ask = if kind == "rect" {
        SelectAsk(
            rect: .some([x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()]),
            mode: .some(mode),
            feather: .some(feather),
            grow: .some(grow),
            invert: .some(invert),
        )
    } else {
        SelectAsk(
            ellipse: .some(
                [(x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0).abs() / 2.0, (y1 - y0).abs() / 2.0],
            ),
            mode: .some(mode),
            feather: .some(feather),
            grow: .some(grow),
            invert: .some(invert),
        )
    }
    ask_selection(a, json.encode(ask))
}

/// Selects the area a lasso traces, closed, meeting the area there is by `mode`.
action select_lasso(
    a: Artwork,
    points: List<List<Float>>,
    mode: Text = "replace",
    feather: Float = 0.0,
    grow: Float = 0.0,
    invert: Bool = false,
) -> Option<Text> uses image (icon: .circle_dashed, label: "Select by lasso") {
    require points.len() >= 3 "A lasso goes round an area"
    let ask = SelectAsk(
        points: .some(points),
        mode: .some(mode),
        feather: .some(feather),
        grow: .some(grow),
        invert: .some(invert),
    )
    ask_selection(a, json.encode(ask))
}

/// Selects the area an outline names, meeting the area there is by `mode`.
action select_outline(
    a: Artwork,
    d: Text,
    mode: Text = "replace",
    feather: Float = 0.0,
    grow: Float = 0.0,
    invert: Bool = false,
) -> Option<Text> uses image (icon: .circle_dashed, label: "Select an outline") {
    require d.trim() != "" "A selection has an outline"
    let ask = SelectAsk(
        d: .some(d),
        mode: .some(mode),
        feather: .some(feather),
        grow: .some(grow),
        invert: .some(invert),
    )
    ask_selection(a, json.encode(ask))
}

/// Grows (or shrinks, when negative) the selection's edge by `by` units.
action grow_selection(a: Artwork, by: Float) -> Option<Text> uses image (
    icon: .circle_dashed,
    label: "Grow the selection",
) {
    if let area = a.selection {
        let ask = SelectAsk(
            d: .some(area.d),
            mode: .some("replace"),
            grow: .some(by),
            feather: .some(area.feather),
        )
        return ask_selection(a, json.encode(ask))
    }
    a.notice = "Select an area first"
    return .none
}

/// Turns the selection inside out: everything in the artwork but what it held.
action invert_selection(a: Artwork) -> Option<Text> uses image (
    icon: .circle_dashed,
    label: "Invert the selection",
) {
    if let area = a.selection {
        let ask = SelectAsk(
            d: .some(area.d),
            mode: .some("replace"),
            invert: .some(true),
            feather: .some(area.feather),
        )
        return ask_selection(a, json.encode(ask))
    }
    a.notice = "Select an area first"
    return .none
}

/// Outlines the selection as a mark on `layer`: a stroke along its edge and no fill, so a person
/// frames what they chose with the paint they hold.
action outline_selection(
    a: Artwork,
    layer: Option<Layer> = .none,
    colour: Text = "#1d1b19",
    width: Float = 2.0,
) -> Option<Mark> (icon: .circle_dashed, label: "Outline the selection") {
    require width > 0.0 and width <= 64.0 "An outline is above zero and up to 64 units"
    if let area = a.selection {
        let on = if let chosen = layer { chosen } else { top_layer(a) }
        let made = a.marks.insert(
            Mark(
                layer: on,
                kind: "path",
                d: area.d,
                colour: "none",
                stroke: colour,
                stroke_width: width,
            ),
        )
        a.notice = ""
        return .some(made)
    }
    a.notice = "Select an area first"
    return .none
}

/// Cuts the picked marks to the selection, so only the part inside it shows.
action clip_picks_to_selection(a: Artwork) (
    icon: .scissors,
    label: "Clip the picks to the selection",
) {
    require a.picked.len() > 0 "Pick the marks to clip"
    if let area = a.selection {
        for m in a.picked {
            m.clip = area.d
        }
        a.notice = ""
    } else {
        a.notice = "Select an area first"
    }
}

/// Selects the shape of the marks or groups `ids` name, meeting the area there is by `mode`.
action select_marks(
    a: Artwork,
    ids: List<Text>,
    mode: Text = "replace",
) -> Option<Text> uses image (icon: .circle_dashed, label: "Select marks") {
    require ids.len() > 0 "Name the marks to select"
    let ask = SelectAsk(marks: .some(ids), mode: .some(mode))
    ask_selection(a, json.encode(ask))
}

/// Picks a mark, or lets it go: combine and measure act on the picked marks.
action pick_mark(a: Artwork, m: Mark, on: Bool = true) (
    icon: .check,
    label: "Pick the mark",
    did: "picked a mark",
) {
    if on {
        if a.picked.find(p -> p == m) == .none {
            a.picked = a.picked + [m]
        }
    } else {
        a.picked = a.picked.filter(p -> p != m)
    }
}

/// Picks the marks, adding any that were not picked.
action pick_marks(a: Artwork, marks: List<Mark>) (label: "Pick the marks", did: "picked marks") {
    for m in marks {
        if a.picked.find(p -> p == m) == .none {
            a.picked = a.picked + [m]
        }
    }
}

/// Lets every picked mark go.
action clear_picked(a: Artwork) (label: "Clear the picked marks") {
    a.picked = []
}

/// Removes the brush strokes a path crosses, on `layer` or every layer: what a stroke eraser
/// does. Answers how many went.
action remove_along(a: Artwork, points: List<List<Float>>, layer: Option<Layer>) -> Int uses image (
    icon: .eraser,
    label: "Remove the strokes along a path",
    did: "removed strokes along a path",
) {
    require points.len() >= 2 "A path has two points"
    let ask = json.encode(
        RemoveAsk(along: points, layer: if let l = layer { .some(l.name) } else { .none }),
    )
    match canvas.remove_along(artwork_json(a), ask) {
        .ok(answer) => match json.parse(answer) {
            .ok(held) => match as_array(held) {
                .some(items) => {
                    let indices = items
                        .map(item -> as_text(item) ?? "")
                        .map(id -> id.slice(5, id.len()).to_int() ?? -1)
                    let gone = a.marks.filter(m -> indices.contains(a.marks.index_of(m) ?? -1))
                    let count = gone.len()
                    for m in gone {
                        a.marks.remove(m)
                    }
                    a.notice = ""
                    count
                }
                .none => 0,
            },
            .err(_) => 0,
        },
        .err(why) => {
            a.notice = why
            0
        }
    }
}

/// Lists what the artwork holds: its layers and newest marks, with ids, bounds and colours,
/// and with `palette` the colours it paints with. The answer is the engine's JSON.
action inspect_artwork(
    a: Artwork,
    palette: Bool = false,
    limit: Int = 100,
    layer: Text = "",
    region: List<Float> = [],
) -> Option<Text> uses image (icon: .search, label: "Inspect the artwork") {
    require limit >= 0 and limit <= 400 "An inspection lists 0 to 400 marks"
    let ask = json.encode(
        InspectAsk(
            palette: .some(palette),
            region: optional_at(region),
            layer: optional_text(layer),
            limit: .some(limit),
        ),
    )
    match canvas.inspect(artwork_json(a), ask) {
        .ok(info) => {
            a.notice = inspect_said(info)
            return .some(info)
        }
        .err(why) => {
            a.notice = why
            return .none
        }
    }
}

fn inspect_said(info: Text) -> Text {
    match json.parse(info) {
        .ok(held) => {
            let layers = array_field(held, "layers").len()
            let marks = array_field(held, "marks").len()
            "{layers} layers, {marks} marks listed"
        }
        .err(_) => "Inspected",
    }
}

/// Sets how the brush lays a mark: its flow, hardness, spacing, smoothing and taper, each 0 to
/// 1. A parameter left out stays as it is.
action set_dynamics(
    m: Mark,
    flow: Float = -1.0,
    hardness: Float = -1.0,
    spacing: Float = -1.0,
    smoothing: Float = -1.0,
    taper: Float = -1.0,
) (icon: .sliders_horizontal, label: "Set the brush dynamics", did: "set brush dynamics") {
    if flow >= 0.0 {
        m.params.put("flow", flow.clamp(0.0, 1.0))
    }
    if hardness >= 0.0 {
        m.params.put("hardness", hardness.clamp(0.0, 1.0))
    }
    if spacing >= 0.0 {
        m.params.put("spacing", spacing.clamp(0.0, 1.0))
    }
    if smoothing >= 0.0 {
        m.params.put("smoothing", smoothing.clamp(0.0, 1.0))
    }
    if taper >= 0.0 {
        m.params.put("taper", taper.clamp(0.0, 1.0))
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
        exposure: l.exposure,
        contrast: l.contrast,
        highlights: l.highlights,
        shadows: l.shadows,
        saturation: l.saturation,
        vibrance: l.vibrance,
        hue: l.hue,
        temperature: l.temperature,
        tint: l.tint,
        grayscale: l.adjust_grayscale,
        invert_adjust: l.adjust_invert,
        blur: l.blur,
        sharpen: l.sharpen,
        vignette: l.vignette,
        curve: l.curve,
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
        stroke: m.stroke,
        stroke_width: m.stroke_width,
        dash: m.dash,
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
        transform: m.transform,
        clip: m.clip,
        pixelated: m.pixelated,
        grain: m.grain,
        feather: m.feather,
        shadow_x: m.shadow_x,
        shadow_y: m.shadow_y,
        shadow_blur: m.shadow_blur,
        shadow_colour: m.shadow_colour,
        inner_shadow_x: m.inner_shadow_x,
        inner_shadow_y: m.inner_shadow_y,
        inner_shadow_blur: m.inner_shadow_blur,
        inner_shadow_colour: m.inner_shadow_colour,
        curve: m.curve,
    )
}

/// The blend a mark mixes with: `normal` for the empty name the records keep.
fn blend_of(blend: Text) -> Text {
    if blend == "" { "normal" } else { blend }
}

fn area_json(a: Artwork) -> Option<AreaJson> {
    if let area = a.selection {
        return .some(
            AreaJson(
                d: area.d,
                feather: area.feather,
                matte: if area.matte_src == "" {
                    .none
                } else {
                    .some(
                        MatteJson(
                            src: area.matte_src,
                            rect: [
                                area.matte_x,
                                area.matte_y,
                                area.matte_width,
                                area.matte_height,
                            ],
                        ),
                    )
                },
            ),
        )
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
action export_picture(
    a: Artwork,
    format: Text = "png",
    scale: Float = 1.0,
    selection: Bool = false,
) -> Option<File> (icon: .download, label: "Export a picture", did: "exported a picture") {
    if selection and a.selection == .none {
        a.notice = "Select an area to export it"
        return .none
    }
    let request = json.encode(
        ExportRequest(format: format, scale: scale, selection: .some(selection)),
    )
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
        let stroke = text_field(row, "stroke")
        let stroke_width = number_field(row, "stroke_width") ?? 0.0
        let dash = numbers_at(row, "dash")
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
                    stroke: stroke,
                    stroke_width: stroke_width,
                    dash: dash,
                ),
            )
        }
        if let d = path_of(shape) {
            return .some(
                Mark(
                    layer: layer,
                    kind: "path",
                    d: d,
                    colour: colour,
                    opacity: opacity,
                    stroke: stroke,
                    stroke_width: stroke_width,
                    dash: dash,
                ),
            )
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

/// A gradient stop's colour, black where the list holds none there.
fn stop_colour(colours: List<Text>, at: Int) -> Text {
    colours.at(at) ?? "#000000"
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
    // The points a lasso has traced, `[x, y]` each, closed when the hand lifts.
    state lasso: List<List<Float>> = []
    state zoom: Float = 1.0
    state pan_x: Float = 0.0
    state pan_y: Float = 0.0
    // The chart being made, and the keyboard reference, each open while its sheet is.
    state chart_open: Bool = false
    state help: Bool = false
    std.page(a, canvas: true, fill: true) {
        header: {
            artwork_head(a, show_layers, chart_open, help)
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
                    overlay: a.version_overlay,
                    overlay_rect: a.version_region,
                    overlay_opacity: 0.7,
                    width: a.width,
                    height: a.height,
                    brush: paint_brush(tool, brush),
                    size: size,
                    colour: colour,
                    zoom: zoom,
                    pan: [pan_x, pan_y],
                    label: a.title,
                ) {
                    on stroke_start(p: List<Float>): set(
                            colour,
                            if tool == "eyedropper" { colour_at(a, p) ?? colour } else { colour },
                        )
                        .then(set(shape_from, p))
                        .then(
                            start_paint(a, tool, paint_brush(tool, brush), chosen, p, size, colour),
                        )
                    on stroke(p: List<Float>): set(lasso, lasso_through(tool, lasso, p))
                        .then(
                            continue_paint(a, tool, paint_brush(tool, brush), p, size, colour),
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
                            lasso,
                        )
                        .then(set(lasso, []))
                    // The wheel and a pinch zoom about the view, and a middle-button drag pans it.
                    on wheel(delta): set(zoom, zoomed(zoom, delta))
                    on pinch(scale): set(zoom, (zoom * scale).clamp(0.05, 64.0))
                    on panned(delta): set(pan_x, pan_x + (delta.at(0) ?? 0.0))
                        .then(set(pan_y, pan_y + (delta.at(1) ?? 0.0)))
                }
                well(a, tool, brush, size, colour, chosen, pan_x, pan_y, zoom, help)
                version_timeline(a)
            }
        }
    }
    if chart_open {
        chart_sheet(a, chart_open)
    }
    if help {
        help_sheet(help)
    }
}

/// The window's own controls: what makes and keeps the artwork, the chart sheet and the help.
view artwork_head(
    a: Artwork,
    show_layers: State<Bool>,
    chart_open: State<Bool>,
    help: State<Bool>,
) {
    ui.Toolbar {
        ui.ToolbarButton("New artwork", icon: .plus, priority: 6) {
            on press: new_artwork(root Library, "Untitled artwork", 1024.0, 768.0)
        }
        ui.ToolbarButton("Add layer", icon: .layers, priority: 5) {
            on press: add_layer(a, "")
        }
        ui.ToolbarButton("Place a picture", icon: .image_plus, priority: 4) {
            on press: file.choose(
                place_picture(a),
                accept: ["image/png", "image/jpeg", "image/webp"],
            )
        }
        ui.ToolbarButton("Set the reference", icon: .image, priority: 3) {
            on press: file.choose(
                set_reference(a),
                accept: ["image/png", "image/jpeg", "image/webp"],
            )
        }
        ui.ToolbarButton("Chart", icon: .bar_chart, priority: 4) {
            on press: set(chart_open, true)
        }
        ui.ToolbarButton("Look", icon: .eye, priority: 2) {
            on press: draw_look(a)
        }
        ui.ToolbarSpacer()
        ui.ToolbarButton("Keyboard shortcuts", icon: .circle_help, priority: 1) {
            on press: set(help, true)
        }
        ui.ToolbarButton("Layers", icon: .panel_left, toggled: show_layers, priority: 6) {
            on press: set(show_layers, not show_layers)
        }
    }
}

/// The chart sheet: the labels and the numbers they stand for, and the title over them. Drawing
/// it makes the bars, their labels and the axis as marks in one group.
view chart_sheet(a: Artwork, open: State<Bool>) {
    state title: Text = ""
    state labels: Text = "Jan, Feb, Mar, Apr"
    state values: Text = "3, 5, 2, 6"
    ui.ModalDialog(
        title: "Draw a chart",
        open: open,
        primary: "Draw the chart",
        secondary: "Cancel",
    ) {
        body: {
            stack(gap: .related) {
                ui.TextInput(bind: title, placeholder: "Title over the chart")
                ui.Text("Labels, one per value", role: .caption, secondary: true)
                ui.TextInput(bind: labels, placeholder: "Jan, Feb, Mar, Apr")
                ui.Text("Values", role: .caption, secondary: true)
                ui.TextInput(bind: values, placeholder: "3, 5, 2, 6")
            }
        }
        on act_primary: add_chart(
                a,
                labels.split(",").map(x -> x.trim()).filter(x -> x != ""),
                values.split(",").map(x -> x.trim().to_float() ?? 0.0),
                title: title,
            )
            .then(set(open, false))
        on act_secondary: set(open, false)
        on dismiss: set(open, false)
    }
}

/// The keyboard reference: what each key does while the artwork has the window.
view help_sheet(open: State<Bool>) {
    ui.ModalDialog(
        title: "Keyboard shortcuts",
        icon: .circle_help,
        open: open,
        secondary: "Close",
    ) {
        on act_secondary: set(open, false)
        on dismiss: set(open, false)
        stack(gap: .related, padding: .related) {
            ui.Text("Tools", role: .heading)
            ui.KeyHints {
                ui.KeyHint(["B"], "Brush")
                ui.KeyHint(["X"], "Eraser")
                ui.KeyHint(["I"], "Eyedropper")
                ui.KeyHint(["R"], "Rectangle")
                ui.KeyHint(["E"], "Ellipse")
                ui.KeyHint(["L"], "Line")
                ui.KeyHint(["C"], "Crop the picture")
                ui.KeyHint(["V"], "Select")
                ui.KeyHint(["Q"], "Lasso")
                ui.KeyHint(["W"], "Wand")
                ui.KeyHint(["T"], "Text")
            }
            ui.Text("View", role: .heading)
            ui.KeyHints {
                ui.KeyHint(["+ and -"], "Zoom in or out")
                ui.KeyHint(["Shift+1"], "Fit the artwork")
                ui.KeyHint(["Shift+0"], "Actual size")
                ui.KeyHint(["[ and ]"], "Smaller or larger brush")
            }
            ui.Text("Marks and artwork", role: .heading)
            ui.KeyHints {
                ui.KeyHint(["Alt+Arrows"], "Nudge the picked marks")
                ui.KeyHint(["Delete"], "Delete the picked marks")
                ui.KeyHint(["Escape"], "Let the picks or the selection go")
                ui.KeyHint(["Ctrl+N"], "New artwork")
                ui.KeyHint(["Ctrl+O"], "Open an artwork")
                ui.KeyHint(["Ctrl+S"], "Save the artwork")
                ui.KeyHint(["Ctrl+Shift+E"], "Export a PNG")
                ui.KeyHint(["?"], "Show or close this reference")
            }
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
                        ui.Button("Up", role: .plain, icon: .chevron_up, icon_only: true) {
                            on press: move_layer_up(a, layer)
                        }
                        ui.Button("Down", role: .plain, icon: .chevron_down, icon_only: true) {
                            on press: move_layer_down(a, layer)
                        }
                    }
                }
            }
        }
        if let layer = chosen {
            row(gap: .tight, align: .center) {
                if let mask = a.layers.find(m -> m.mask_of == layer.name) {
                    ui.Button(
                        if mask.invert { "Light hides" } else { "Light shows" },
                        icon: .contrast,
                    ) {
                        on press: set_mask(a, mask, invert: not mask.invert)
                    }
                    ui.Button("Remove the mask", icon: .trash) {
                        on press: remove_layer(a, mask)
                    }
                } else if layer.mask_of == "" {
                    ui.Button("Add a mask", icon: .contrast) {
                        on press: add_mask(a, layer)
                    }
                }
                ui.Button(if layer.locked { "Unlock" } else { "Lock" }, icon: .lock) {
                    on press: lock_layer(a, layer)
                }
            }
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
                ui.Button("Pick", icon: .check) {
                    on press: pick_marks(a, marks)
                }
            }
            if a.picked.len() > 0 {
                ui.Text("{a.picked.len()} picked", role: .caption, secondary: true)
                row(gap: .tight, align: .center) {
                    ui.Text("Nudge", role: .caption, secondary: true)
                    ui.Button(
                        "Left",
                        role: .plain,
                        icon: .arrow_left,
                        icon_only: true,
                        keys: ["Alt+ArrowLeft"],
                    ) {
                        on press: nudge_picked(a, -1.0, 0.0)
                    }
                    ui.Button(
                        "Up",
                        role: .plain,
                        icon: .arrow_up,
                        icon_only: true,
                        keys: ["Alt+ArrowUp"],
                    ) {
                        on press: nudge_picked(a, 0.0, -1.0)
                    }
                    ui.Button(
                        "Down",
                        role: .plain,
                        icon: .arrow_down,
                        icon_only: true,
                        keys: ["Alt+ArrowDown"],
                    ) {
                        on press: nudge_picked(a, 0.0, 1.0)
                    }
                    ui.Button(
                        "Right",
                        role: .plain,
                        icon: .arrow_right,
                        icon_only: true,
                        keys: ["Alt+ArrowRight"],
                    ) {
                        on press: nudge_picked(a, 1.0, 0.0)
                    }
                }
                if a.picked.len() >= 2 {
                    ui.MenuButton("Align", icon: .align_start_horizontal) {
                        ui.MenuItem("Align left edges") {
                            on choose: align_marks(a, a.picked, "left")
                        }
                        ui.MenuItem("Align centres") {
                            on choose: align_marks(a, a.picked, "center")
                        }
                        ui.MenuItem("Align right edges") {
                            on choose: align_marks(a, a.picked, "right")
                        }
                        ui.MenuItem("Align top edges") {
                            on choose: align_marks(a, a.picked, "top")
                        }
                        ui.MenuItem("Align middles") {
                            on choose: align_marks(a, a.picked, "middle")
                        }
                        ui.MenuItem("Align bottom edges") {
                            on choose: align_marks(a, a.picked, "bottom")
                        }
                        ui.MenuSeparator()
                        ui.MenuItem("Space evenly across") {
                            on choose: align_marks(a, a.picked, distribute: "horizontal")
                        }
                        ui.MenuItem("Space evenly down") {
                            on choose: align_marks(a, a.picked, distribute: "vertical")
                        }
                    }
                }
                ui.MenuButton("Align on the artwork", icon: .maximize) {
                    ui.MenuItem("Centre across the artwork") {
                        on choose: align_marks(a, a.picked, "center", to_artwork: true)
                    }
                    ui.MenuItem("Centre down the artwork") {
                        on choose: align_marks(a, a.picked, "middle", to_artwork: true)
                    }
                    ui.MenuItem("Left edge of the artwork") {
                        on choose: align_marks(a, a.picked, "left", to_artwork: true)
                    }
                    ui.MenuItem("Right edge of the artwork") {
                        on choose: align_marks(a, a.picked, "right", to_artwork: true)
                    }
                    ui.MenuItem("Top edge of the artwork") {
                        on choose: align_marks(a, a.picked, "top", to_artwork: true)
                    }
                    ui.MenuItem("Bottom edge of the artwork") {
                        on choose: align_marks(a, a.picked, "bottom", to_artwork: true)
                    }
                }
                ui.Button(
                    "Delete the picked marks",
                    icon: .trash,
                    role: .destructive,
                    keys: ["Delete"],
                ) {
                    on press: delete_picked(a)
                }
                ui.Button("Let the picks go", icon: .x, keys: ["Escape"]) {
                    on press: clear_picked(a)
                }
            }
        }
        if let m = a.marks.last() {
            ui.Section("Brush dynamics") {
                ui.Slider(value: m.params.get("flow") ?? 1.0, label: "Flow", min: 0.0, max: 1.0) {
                    on change(v): set_dynamics(m, flow: v)
                }
                ui.Slider(
                    value: m.params.get("hardness") ?? 1.0,
                    label: "Hardness",
                    min: 0.0,
                    max: 1.0,
                ) {
                    on change(v): set_dynamics(m, hardness: v)
                }
                ui.Slider(
                    value: m.params.get("spacing") ?? 0.0,
                    label: "Spacing",
                    min: 0.0,
                    max: 1.0,
                ) {
                    on change(v): set_dynamics(m, spacing: v)
                }
                ui.Slider(
                    value: m.params.get("smoothing") ?? 0.0,
                    label: "Smoothing",
                    min: 0.0,
                    max: 1.0,
                ) {
                    on change(v): set_dynamics(m, smoothing: v)
                }
                ui.Slider(value: m.params.get("taper") ?? 0.0, label: "Taper", min: 0.0, max: 1.0) {
                    on change(v): set_dynamics(m, taper: v)
                }
            }
            ui.Section("Outline") {
                row(gap: .tight, align: .center) {
                    ui.Button("Solid", toggled: m.dash.is_empty()) {
                        on press: set_dash(m, [])
                    }
                    ui.Button("Dashed", toggled: m.dash == [8.0, 4.0]) {
                        on press: set_dash(m, [8.0, 4.0])
                    }
                    ui.Button("Dotted", toggled: m.dash == [1.0, 3.0]) {
                        on press: set_dash(m, [1.0, 3.0])
                    }
                }
                ui.Slider(bind: m.stroke_width, label: "Outline width", min: 0.0, max: 24.0)
                ui.MenuButton("Outline colour", icon: .droplet) {
                    for ink in inks() {
                        ui.MenuItem(ink) {
                            on choose: set_outline(
                                m,
                                ink,
                                if m.stroke_width > 0.0 { m.stroke_width } else { 2.0 },
                            )
                        }
                    }
                    ui.MenuItem("No outline") {
                        on choose: set_outline(m, "", 0.0)
                    }
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
        ui.Text("Line height", role: .caption, secondary: true)
        ui.Slider(bind: t.line_height, label: "Line height", min: 0.0, max: 4.0)
        ui.Text("Tracking", role: .caption, secondary: true)
        ui.Slider(bind: t.tracking, label: "Tracking", min: -1.0, max: 10.0)
        ui.Text("Set along a path, as SVG path data", role: .caption, secondary: true)
        ui.TextInput(bind: t.text_path)
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
    chosen: State<Option<Layer>>,
    pan_x: State<Float>,
    pan_y: State<Float>,
    zoom: State<Float>,
    help: State<Bool>,
) {
    row(gap: .related, align: .center, wrap: true) {
        // The tools, each answering its letter while the artwork has the window.
        row(gap: .tight, align: .center, wrap: true) {
            ui.Button(
                "Brush",
                role: .plain,
                icon: .pencil,
                icon_only: true,
                toggled: tool == "brush",
                keys: ["b"],
            ) {
                on press: set(tool, "brush")
            }
            ui.Button(
                "Eraser",
                role: .plain,
                icon: .eraser,
                icon_only: true,
                toggled: tool == "eraser",
                keys: ["x"],
            ) {
                on press: set(tool, "eraser")
            }
            ui.Button(
                "Eyedropper",
                role: .plain,
                icon: .pipette,
                icon_only: true,
                toggled: tool == "eyedropper",
                keys: ["i"],
            ) {
                on press: set(tool, "eyedropper")
            }
            ui.Button(
                "Rectangle",
                role: .plain,
                icon: .square,
                icon_only: true,
                toggled: tool == "rect",
                keys: ["r"],
            ) {
                on press: set(tool, "rect")
            }
            ui.Button(
                "Ellipse",
                role: .plain,
                icon: .circle,
                icon_only: true,
                toggled: tool == "ellipse",
                keys: ["e"],
            ) {
                on press: set(tool, "ellipse")
            }
            ui.Button(
                "Line",
                role: .plain,
                icon: .minus,
                icon_only: true,
                toggled: tool == "line",
                keys: ["l"],
            ) {
                on press: set(tool, "line")
            }
            ui.Button(
                "Crop",
                role: .plain,
                icon: .crop,
                icon_only: true,
                toggled: tool == "crop",
                keys: ["c"],
            ) {
                on press: set(tool, "crop")
            }
            ui.Button(
                "Select",
                role: .plain,
                icon: .circle_dashed,
                icon_only: true,
                toggled: tool == "select",
                keys: ["v"],
            ) {
                on press: set(tool, "select")
            }
            ui.Button(
                "Lasso",
                role: .plain,
                icon: .lasso,
                icon_only: true,
                toggled: tool == "lasso",
                keys: ["q"],
            ) {
                on press: set(tool, "lasso")
            }
            ui.Button(
                "Wand",
                role: .plain,
                icon: .wand_sparkles,
                icon_only: true,
                toggled: tool == "wand",
                keys: ["w"],
            ) {
                on press: set(tool, "wand")
            }
            ui.Button(
                "Text",
                role: .plain,
                icon: .type,
                icon_only: true,
                toggled: tool == "text",
                keys: ["t"],
            ) {
                on press: set(tool, "text")
            }
        }
        ui.MenuButton("Brush: {brush}", icon: .pencil) {
            for preset in brushes() {
                ui.MenuItem(preset) {
                    on choose: set(brush, preset)
                }
            }
        }
        // What the next stroke lays down: the brush, its size and the paint, drawn as they change.
        ui.StrokePreview(paint_brush(tool, brush), size: size, colour: colour)
        ui.Button("Smaller brush", role: .plain, icon: .minus, icon_only: true, keys: ["["]) {
            on press: set(size, (size - 1.0).max(1.0))
        }
        ui.Slider(bind: size, label: "Size", min: 1.0, max: 128.0, fill)
        ui.Button("Larger brush", role: .plain, icon: .plus, icon_only: true, keys: ["]"]) {
            on press: set(size, (size + 1.0).min(128.0))
        }
        ui.ColorPicker(bind: colour, swatches: inks(), columns: 7, label: "Paint")
        if let layer = chosen {
            if let medium = a.media.find(m -> m.key == layer.medium) {
                if medium.palette.len() > 0 {
                    ui.MenuButton("Pigment", icon: .droplet) {
                        for p in medium.palette {
                            ui.MenuItem(p.name) {
                                on choose: set(colour, p.colour)
                            }
                        }
                    }
                }
            }
        }
        if tool == "select" {
            ui.Button("Fill the selection", icon: .paint_bucket) {
                on press: fill_selection(a, .none, colour)
            }
            if a.selection != .none {
                ui.Button("Outline the selection", icon: .circle_dashed) {
                    on press: outline_selection(a, .none, colour, 2.0)
                }
            }
            if a.picked.len() > 0 and a.selection != .none {
                ui.Button("Clip the picks to the selection", icon: .scissors) {
                    on press: clip_picks_to_selection(a)
                }
            }
            ui.Button("Deselect", icon: .circle_dashed, keys: ["Escape"]) {
                on press: clear_selection(a)
            }
        }
        ui.MenuButton("File", icon: .save) {
            ui.MenuItem("Save the artwork", icon: .save, key: "Ctrl+S") {
                on choose: save_artwork(a)
            }
            ui.MenuItem("PNG, full size", key: "Ctrl+Shift+E") {
                on choose: file.save(export_picture(a, "png", 1.0), name: "Artwork.png")
            }
            if a.selection != .none {
                ui.MenuItem("PNG of the selection") {
                    on choose: file.save(export_picture(a, "png", 1.0, true), name: "Selection.png")
                }
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
        if let shape = last_shape(a) {
            if shape.gradient != "" {
                ui.Section("Gradient stops") {
                    let offsets = shape.gradient_offsets ?? []
                    let colours = shape.gradient_colours ?? []
                    for i in offsets.map((o, i) -> i) {
                        row(gap: .tight, align: .center) {
                            ui.Slider(
                                value: offsets.at(i) ?? 0.0,
                                label: "Stop {i + 1}",
                                min: 0.0,
                                max: 1.0,
                            ) {
                                on change(v): set_gradient_offset(shape, i, v)
                            }
                            ui.MenuButton(stop_colour(colours, i), icon: .droplet) {
                                for ink in inks() {
                                    ui.MenuItem(ink) {
                                        on choose: set_gradient_colour(shape, i, ink)
                                    }
                                }
                            }
                        }
                    }
                    row(gap: .tight, align: .center) {
                        ui.Button("Add a stop", icon: .plus) {
                            on press: add_gradient_stop(shape)
                        }
                        ui.Button("Remove a stop", icon: .minus) {
                            on press: remove_gradient_stop(shape, (offsets.len() - 1).max(0))
                        }
                    }
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
        ui.Button("Zoom out", role: .plain, icon: .zoom_out, icon_only: true, keys: ["-"]) {
            on press: set(zoom, (zoom / 1.25).max(0.05))
        }
        ui.Text("{zoomed_said(zoom)}", role: .caption, secondary: true)
        ui.Button("Zoom in", role: .plain, icon: .zoom_in, icon_only: true, keys: ["+"]) {
            on press: set(zoom, (zoom * 1.25).min(64.0))
        }
        ui.Button(
            "Actual size",
            role: .plain,
            icon: .layout_grid,
            icon_only: true,
            keys: ["Shift+0"],
        ) {
            on press: set(zoom, 1.0)
        }
        ui.Button(
            "Fit the view",
            role: .plain,
            icon: .maximize,
            icon_only: true,
            keys: ["Shift+1"],
        ) {
            on press: set(zoom, 1.0).then(set(pan_x, 0.0)).then(set(pan_y, 0.0))
        }
        ui.Button(
            "Keyboard shortcuts",
            role: .plain,
            icon: .circle_help,
            icon_only: true,
            keys: ["?"],
        ) {
            on press: set(help, true)
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
    if tool == "lasso" {
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
    lasso: List<List<Float>> = [],
) -> Option<Mark> {
    if tool == "lasso" {
        if lasso.len() >= 3 { select_lasso(a, lasso) }
        return .none
    }
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
    if tool == "crop" and not same_place(from, point) {
        if let p = last_picture(a) {
            let area = dragged_crop(p, from, point)
            let width = area.at(2) ?? 0.0
            let height = area.at(3) ?? 0.0
            if width > 0.02 and height > 0.02 {
                crop_picture(p, area.at(0) ?? 0.0, area.at(1) ?? 0.0, width, height)
            }
        }
    }
    if tool == "select" and not same_place(from, point) { select_area(a, rect_path(from, point)) }
    .none
}

/// Where a crop drag over the picture `p` crops it, as fractions of the picture's own pixels:
/// the drag's corners inside the frame the picture is drawn in, laid over the crop it has.
fn dragged_crop(p: Mark, from: List<Float>, to: List<Float>) -> List<Float> {
    let fx0 = from.at(0) ?? 0.0
    let fy0 = from.at(1) ?? 0.0
    let fx1 = to.at(0) ?? 0.0
    let fy1 = to.at(1) ?? 0.0
    let left = fx0.min(fx1)
    let top = fy0.min(fy1)
    let right = fx0.max(fx1)
    let bottom = fy0.max(fy1)
    let width = p.width.max(0.0001)
    let height = p.height.max(0.0001)
    let x = ((left - p.x) / width).clamp(0.0, 1.0)
    let y = ((top - p.y) / height).clamp(0.0, 1.0)
    let w = ((right - p.x) / width).clamp(0.0, 1.0) - x
    let h = ((bottom - p.y) / height).clamp(0.0, 1.0) - y
    [p.crop_x + x * p.crop_width, p.crop_y + y * p.crop_height, w * p.crop_width, h * p.crop_height]
}

/// The lasso's points with `p` added while the lasso tool draws; unchanged for any other tool.
fn lasso_through(tool: Text, lasso: List<List<Float>>, p: List<Float>) -> List<List<Float>> {
    if tool == "lasso" {
        lasso + [[p.at(0) ?? 0.0, p.at(1) ?? 0.0]]
    } else { lasso }
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
