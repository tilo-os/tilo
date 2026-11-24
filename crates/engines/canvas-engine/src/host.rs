//! The engine as a device answers a package's `canvas.*`: artwork JSON in, a picture's bytes or
//! JSON out. No window, no resource, no history: callers that keep the artwork as records call
//! these to see it and to carry it over the two files it saves.
//!
//! Pictures the artwork names travel with the JSON as `(address, bytes)` pairs. Rendering lends
//! them to the engine's content store for the call, so a look or an export shows them exactly as
//! the window would.

use std::collections::BTreeMap;

use tilo_content::ContentStore;
use tilo_ui::text_render::TextRenderer;

use crate::document::{ArtworkFile, pack, unpack};
use crate::model::{Adjust, Artwork, Ground, Layer, Mark, Selection, Shape, Style};
use crate::render::Renderer;
use crate::resource::{Compare, Export, ExportSvg, Look};
use crate::svg::ImportSvg;
use crate::{svg, view_ops};

/// The artwork `json` holds, as the window keeps it (its live shape, not the file's).
///
/// # Errors
///
/// Why the text is not an artwork.
pub fn artwork(json: &str) -> Result<Artwork, String> {
    // A package names a picture by the address its language gives a file
    // (`tilo-blob://`); the engine names the same bytes by its resource address.
    let json = json.replace("tilo-blob://", "tilo.content:");
    let file = ArtworkFile::parse(json.as_bytes())?;
    artwork_of(&file)
}

/// The content reference `address` names, whichever of the two address forms it is written in.
fn reference(address: &str) -> Option<&str> {
    if let Some(bare) = address.strip_prefix("tilo-blob://") {
        return tilo_content::valid_reference(bare).then_some(bare);
    }
    tilo_resource::content::reference_hash(address)
        .or_else(|| tilo_content::valid_reference(address).then_some(address))
}

/// The artwork a package keeps as its own records, read into the engine's: the shape
/// `apps/tl/canvas` encodes, never a second copy of the engine's fields.
///
/// # Errors
///
/// Why the records are not an artwork.
pub fn artwork_of_records(json: &str) -> Result<Artwork, String> {
    #[derive(serde::Deserialize)]
    struct Records {
        #[serde(default)]
        title: String,
        #[serde(default = "wide")]
        width: f64,
        #[serde(default = "tall")]
        height: f64,
        #[serde(default)]
        ground_texture: String,
        #[serde(default)]
        ground_colour: String,
        #[serde(default)]
        layers: Vec<RecordLayer>,
        /// Media presets saved in the artwork.
        #[serde(default)]
        media: Vec<RecordMedium>,
        #[serde(default)]
        marks: Vec<RecordMark>,
        #[serde(default)]
        selection: Option<RecordArea>,
        /// The picture the artwork is compared against; never part of the work.
        #[serde(default)]
        reference: Option<RecordReference>,
    }
    #[derive(serde::Deserialize)]
    struct RecordReference {
        #[serde(default)]
        image: String,
        #[serde(default)]
        frame: Option<[f64; 4]>,
        #[serde(default = "one")]
        opacity: f64,
        #[serde(default = "yes")]
        visible: bool,
    }
    impl Default for RecordLayer {
        fn default() -> Self {
            Self {
                name: String::new(),
                group: String::new(),
                mask_of: String::new(),
                base: one(),
                invert: false,
                clip: false,
                unlinked: false,
                medium: String::new(),
                opacity: one(),
                visible: yes(),
                locked: false,
                texture: 0.0,
            }
        }
    }
    #[derive(serde::Deserialize)]
    struct RecordLayer {
        #[serde(default)]
        name: String,
        /// Organisational folder of layers; empty for an ungrouped layer.
        #[serde(default)]
        group: String,
        /// The layer's name this layer masks; empty for an ordinary layer.
        #[serde(default)]
        mask_of: String,
        /// A mask's grey before any mark.
        #[serde(default = "one")]
        base: f64,
        #[serde(default)]
        invert: bool,
        /// Clipped to the layer below.
        #[serde(default)]
        clip: bool,
        #[serde(default)]
        unlinked: bool,
        #[serde(default)]
        medium: String,
        #[serde(default = "one")]
        opacity: f64,
        #[serde(default = "yes")]
        visible: bool,
        #[serde(default)]
        locked: bool,
        #[serde(default)]
        texture: f64,
    }
    #[derive(serde::Deserialize)]
    struct RecordMedium {
        #[serde(default)]
        id: String,
        #[serde(default)]
        label: String,
        /// `oil` or `watercolour`.
        #[serde(default)]
        engine: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        params: BTreeMap<String, f64>,
        #[serde(default)]
        ground: Option<String>,
        #[serde(default)]
        palette: Vec<RecordPigment>,
    }
    #[derive(serde::Deserialize)]
    struct RecordPigment {
        #[serde(default)]
        name: String,
        #[serde(default)]
        colour: String,
        #[serde(default)]
        granulation: f64,
        #[serde(default = "one")]
        strength: f64,
    }
    #[derive(serde::Deserialize)]
    struct RecordMark {
        #[serde(default)]
        name: String,
        #[serde(default)]
        layer: RecordLayer,
        #[serde(default)]
        kind: String,
        /// Organisational folder of marks; empty for ungrouped.
        #[serde(default)]
        group: String,
        /// `x, y, pressure` in turn, as a package keeps a stroke.
        #[serde(default)]
        points: Vec<f64>,
        #[serde(default)]
        d: String,
        #[serde(default = "width_default")]
        size: f64,
        #[serde(default)]
        colour: String,
        #[serde(default)]
        brush: String,
        #[serde(default = "one")]
        opacity: f64,
        /// A gradient fill, drawn instead of the plain colour: `linear` or `radial`, its ends and
        /// its stops.
        #[serde(default)]
        gradient_kind: String,
        #[serde(default)]
        gradient_x0: f64,
        #[serde(default)]
        gradient_y0: f64,
        #[serde(default)]
        gradient_x1: f64,
        #[serde(default)]
        gradient_y1: f64,
        #[serde(default)]
        gradient_offsets: Vec<f64>,
        #[serde(default)]
        gradient_colours: Vec<String>,
        /// A text mark: its words, where its first baseline starts, its type and its setting.
        #[serde(default)]
        text: String,
        #[serde(default)]
        font: String,
        #[serde(default)]
        weight: i64,
        #[serde(default)]
        italic: bool,
        #[serde(default)]
        align: String,
        #[serde(default)]
        line_height: f64,
        #[serde(default)]
        tracking: f64,
        #[serde(default)]
        text_path: String,
        /// On a painted layer: paint into what is still wet, and the medium's parameter
        /// overrides.
        #[serde(default)]
        wet: bool,
        #[serde(default)]
        params: BTreeMap<String, f64>,
        /// A picture's bytes, by the address it is kept at, and the frame it is drawn in.
        #[serde(default)]
        src: String,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
        #[serde(default)]
        width: f64,
        #[serde(default)]
        height: f64,
        /// How the mark mixes with what is beneath it; empty or `normal` is plain.
        #[serde(default)]
        blend: String,
        /// The picture's frame inside its own pixels, as fractions of them.
        #[serde(default)]
        crop_x: f64,
        #[serde(default)]
        crop_y: f64,
        #[serde(default = "one")]
        crop_width: f64,
        #[serde(default = "one")]
        crop_height: f64,
        /// A turn inside the frame, in degrees.
        #[serde(default)]
        straighten: f64,
        #[serde(default)]
        flip_h: bool,
        #[serde(default)]
        flip_v: bool,
        /// Non-destructive colour and tone adjustments of the mark.
        #[serde(default)]
        exposure: f64,
        #[serde(default)]
        contrast: f64,
        #[serde(default)]
        highlights: f64,
        #[serde(default)]
        shadows: f64,
        #[serde(default)]
        saturation: f64,
        #[serde(default)]
        vibrance: f64,
        #[serde(default)]
        hue: f64,
        #[serde(default)]
        temperature: f64,
        #[serde(default)]
        tint: f64,
        #[serde(default)]
        grayscale: bool,
        #[serde(default)]
        invert: bool,
        #[serde(default)]
        blur: f64,
        #[serde(default)]
        sharpen: f64,
        #[serde(default)]
        vignette: f64,
    }
    #[derive(serde::Deserialize)]
    struct RecordArea {
        #[serde(default)]
        d: String,
        #[serde(default)]
        feather: f64,
    }
    fn wide() -> f64 {
        1024.0
    }
    fn tall() -> f64 {
        768.0
    }
    fn one() -> f64 {
        1.0
    }
    fn yes() -> bool {
        true
    }
    fn width_default() -> f64 {
        6.0
    }
    fn slug(name: &str) -> String {
        let mut out = String::new();
        for ch in name.trim().to_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
            } else if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        }
        let out = out.trim_matches('-').to_owned();
        if out.is_empty() {
            "layer".to_owned()
        } else {
            out
        }
    }
    fn order_key(index: usize) -> String {
        format!("{index:08}")
    }
    /// An adjustment's step, absent when it does nothing.
    fn set(value: f64) -> Option<f64> {
        (value.abs() > f64::EPSILON).then_some(value)
    }
    fn adjust_of(mark: &RecordMark) -> Option<Adjust> {
        let adjust = Adjust {
            exposure: set(mark.exposure),
            contrast: set(mark.contrast),
            highlights: set(mark.highlights),
            shadows: set(mark.shadows),
            saturation: set(mark.saturation),
            vibrance: set(mark.vibrance),
            hue: set(mark.hue),
            temperature: set(mark.temperature),
            tint: set(mark.tint),
            curve: None,
            grayscale: mark.grayscale.then_some(true),
            invert: mark.invert.then_some(true),
            blur: set(mark.blur),
            sharpen: set(mark.sharpen),
            vignette: set(mark.vignette),
        };
        (adjust.exposure.is_some()
            || adjust.contrast.is_some()
            || adjust.highlights.is_some()
            || adjust.shadows.is_some()
            || adjust.saturation.is_some()
            || adjust.vibrance.is_some()
            || adjust.hue.is_some()
            || adjust.temperature.is_some()
            || adjust.tint.is_some()
            || adjust.grayscale.is_some()
            || adjust.invert.is_some()
            || adjust.blur.is_some()
            || adjust.sharpen.is_some()
            || adjust.vignette.is_some())
        .then_some(adjust)
    }
    /// The picture's crop, when it shows less than all of it.
    fn crop_of(mark: &RecordMark) -> Option<[f64; 4]> {
        let crop = [mark.crop_x, mark.crop_y, mark.crop_width, mark.crop_height];
        let full = [
            crop[0].abs() < f64::EPSILON,
            crop[1].abs() < f64::EPSILON,
            (crop[2] - 1.0).abs() < f64::EPSILON,
            (crop[3] - 1.0).abs() < f64::EPSILON,
        ];
        (!full.iter().all(|held| *held)).then_some(crop)
    }
    /// A picture turned in its frame, in degrees.
    fn straighten_of(mark: &RecordMark) -> Option<f64> {
        set(mark.straighten)
    }
    /// A flip about the middle of the picture's frame, as an affine transform.
    fn flip_of(mark: &RecordMark) -> Option<[f64; 6]> {
        if !mark.flip_h && !mark.flip_v {
            return None;
        }
        let sx = if mark.flip_h { -1.0 } else { 1.0 };
        let sy = if mark.flip_v { -1.0 } else { 1.0 };
        let cx = mark.x + mark.width / 2.0;
        let cy = mark.y + mark.height / 2.0;
        Some([sx, 0.0, 0.0, sy, cx - sx * cx, cy - sy * cy])
    }
    let records: Records = serde_json::from_str(json)
        .map_err(|error| format!("this is not a Canvas artwork: {error}"))?;
    let mut art = Artwork::default();
    art.title = records.title;
    if records.width >= 16.0 && records.width <= 8192.0 {
        art.width = records.width;
    }
    if records.height >= 16.0 && records.height <= 8192.0 {
        art.height = records.height;
    }
    art.ground = Ground {
        texture: if records.ground_texture.is_empty() {
            "paper".to_owned()
        } else {
            records.ground_texture
        },
        colour: if records.ground_colour.is_empty() {
            "#f4efe3".to_owned()
        } else {
            records.ground_colour
        },
    };
    for (index, layer) in records.layers.iter().enumerate() {
        art.put_layer(Layer {
            id: slug(&layer.name),
            name: layer.name.clone(),
            group: layer.group.clone(),
            order: order_key(index),
            medium: layer.medium.clone(),
            opacity: layer.opacity.clamp(0.0, 1.0),
            visible: layer.visible,
            texture: layer.texture.clamp(0.0, 1.0),
            locked: layer.locked,
            adjust: None,
            mask_of: slug(&layer.mask_of),
            base: layer.base.clamp(0.0, 1.0),
            invert: layer.invert,
            clip: layer.clip,
            unlinked: layer.unlinked,
        });
    }
    if records.layers.is_empty() {
        art.put_layer(Layer {
            id: "layer-1".to_owned(),
            name: "Layer 1".to_owned(),
            order: order_key(0),
            ..Layer::default()
        });
    }
    for medium in &records.media {
        let spec = crate::media::MediumSpec {
            id: medium.id.clone(),
            label: medium.label.clone(),
            engine: medium.engine.clone(),
            description: medium.description.clone(),
            params: medium.params.clone(),
            ground: medium.ground.clone(),
            palette: medium
                .palette
                .iter()
                .map(|pigment| crate::media::PigmentSpec {
                    name: pigment.name.clone(),
                    colour: pigment.colour.clone(),
                    granulation: pigment.granulation.clamp(0.0, 1.0),
                    strength: pigment.strength,
                })
                .collect(),
        };
        art.media.insert(medium.id.clone(), spec);
    }
    if let Some(reference) = &records.reference {
        if !reference.image.is_empty() {
            art.reference = Some(crate::model::Reference {
                image: reference.image.clone(),
                frame: reference.frame,
                opacity: reference.opacity.clamp(0.0, 1.0),
                visible: reference.visible,
            });
        }
    }
    for (index, mark) in records.marks.iter().enumerate() {
        let mut style = Style {
            opacity: Some(mark.opacity.clamp(0.0, 1.0)),
            brush: crate::brushes::stored(&mark.brush),
            ..Style::default()
        };
        let colour = if mark.colour.is_empty() {
            "#1d1b19".to_owned()
        } else {
            mark.colour.clone()
        };
        let shape = if mark.kind == "stroke" {
            // The ink the hand paints in is the stroke's colour; a shape is filled with it.
            style.stroke = Some(colour);
            Shape::Stroke {
                points: mark.points.chunks_exact(3).map(<[f64]>::to_vec).collect(),
                size: mark.size,
                forks: None,
                spread: None,
                dabs: None,
                scatter: None,
                dab_length: None,
                hatch: None,
            }
        } else if mark.kind == "text" {
            style.fill = Some(colour);
            Shape::Text {
                x: mark.x,
                y: mark.y,
                text: mark.text.clone(),
                size: mark.size,
                font: (!mark.font.is_empty()).then(|| mark.font.clone()),
                weight: u16::try_from(mark.weight).ok().filter(|weight| *weight > 0),
                italic: mark.italic,
                align: (!mark.align.is_empty()).then(|| mark.align.clone()),
                line_height: set(mark.line_height),
                width: None,
                tracking: set(mark.tracking),
                path: (!mark.text_path.is_empty()).then(|| mark.text_path.clone()),
            }
        } else if mark.kind == "image" {
            Shape::Image {
                src: mark.src.clone(),
                x: mark.x,
                y: mark.y,
                width: mark.width,
                height: mark.height,
                crop: crop_of(mark),
                straighten: straighten_of(mark),
            }
        } else {
            style.fill = Some(colour);
            Shape::Path { d: mark.d.clone() }
        };
        if !mark.blend.is_empty() && mark.blend != "normal" {
            style.blend = Some(mark.blend.clone());
        }
        style.wet = mark.wet;
        style.params = mark.params.clone();
        if !mark.gradient_kind.is_empty() && mark.gradient_offsets.len() >= 2 {
            let count = mark.gradient_offsets.len().min(mark.gradient_colours.len());
            style.gradient = Some(crate::model::Gradient {
                kind: mark.gradient_kind.clone(),
                from: [mark.gradient_x0, mark.gradient_y0],
                to: [mark.gradient_x1, mark.gradient_y1],
                stops: mark
                    .gradient_offsets
                    .iter()
                    .zip(mark.gradient_colours.iter())
                    .take(count)
                    .map(|(at, colour)| (*at, colour.clone()))
                    .collect(),
                focal: None,
            });
        }
        let adjust = adjust_of(mark);
        style.adjust = adjust;
        art.put_mark(Mark {
            id: format!("mark-{index}"),
            layer: slug(&mark.layer.name),
            order: order_key(index),
            name: mark.name.clone(),
            shape,
            style,
            transform: flip_of(mark),
            group: mark.group.clone(),
            clip: String::new(),
            area: String::new(),
        });
    }
    if let Some(area) = records.selection {
        art.selection = Some(Selection {
            d: area.d,
            feather: area.feather.clamp(0.0, 500.0),
            matte: None,
        });
    }
    art.settle();
    Ok(art)
}

/// The artwork `file` holds, as a fresh document.
///
/// # Errors
///
/// Why the file cannot be a document (a mark on a layer that is gone, too many marks).
pub fn artwork_of(file: &ArtworkFile) -> Result<Artwork, String> {
    let mut art = Artwork::default();
    crate::actions::replace(&mut art, file).map_err(|refusal| refusal.to_string())?;
    Ok(art)
}

/// Run `build` with `pictures` lent to the engine's content stores.
fn lending<T>(pictures: &[(String, Vec<u8>)], build: impl FnOnce() -> T) -> T {
    let scope = tilo_wake::WakeScope::new();
    let store = ContentStore::memory();
    for (address, bytes) in pictures {
        if let Some(reference) = reference(address) {
            let _ = store.put_as(reference, bytes);
        }
    }
    tilo_resource::content::register(&scope.handle(), store.clone());
    let _entered = scope.enter();
    tilo_ui::pictures::lend_content(Some(store));
    let made = build();
    tilo_ui::pictures::lend_content(None);
    made
}

/// `picture` with the grade `adjust` names applied, as PNG bytes.
///
/// # Errors
///
/// Why the picture cannot be decoded, the grade read, or the result encoded.
pub fn grade(picture: &[u8], adjust: &str) -> Result<Vec<u8>, String> {
    use tilo_ui::image_codec::{Encoding, decode_now, encode_now};
    let adjust: crate::model::Adjust = serde_json::from_str(adjust)
        .map_err(|error| format!("a grade's fields cannot be read: {error}"))?;
    let mut image = decode_now(picture, None)
        .map_err(|error| format!("the picture cannot be read: {error}"))?;
    if let Some(grade) = adjust.grade() {
        crate::adjust::grade_pixels(&mut image.rgba, &[grade]);
    }
    encode_now(&image, Encoding::Png)
        .map_err(|error| format!("the graded picture cannot be written: {error}"))
}

/// The area of `picture` like `request` names it, as SVG path data: the magic wand over one
/// picture, at one point or one colour.
///
/// # Errors
///
/// Why the picture cannot be decoded, the request read, or nothing is found.
pub fn wand(picture: &[u8], request: &str) -> Result<String, String> {
    use tilo_ui::image_codec::decode_now;
    #[derive(serde::Deserialize)]
    struct Find {
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        colour: Option<String>,
        #[serde(default)]
        tolerance: Option<f64>,
        #[serde(default)]
        everywhere: Option<bool>,
        /// `[x, y, width, height]` in the artwork: the frame the picture stands in, so the
        /// found area answers in the artwork's own units.
        #[serde(default)]
        frame: Option<[f64; 4]>,
    }
    let find: Find = serde_json::from_str(request)
        .map_err(|error| format!("what to find cannot be read: {error}"))?;
    if find.at.is_none() && find.colour.is_none() {
        return Err("give at (a point) or colour: what to find".to_owned());
    }
    let image = decode_now(picture, None)
        .map_err(|error| format!("the picture cannot be read: {error}"))?;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a point outside the picture is clamped to its pixels"
    )]
    let seed = find.at.map(|at| {
        (
            at[0].max(0.0).min(f64::from(image.width.saturating_sub(1))) as usize,
            at[1]
                .max(0.0)
                .min(f64::from(image.height.saturating_sub(1))) as usize,
        )
    });
    let colour = find
        .colour
        .as_deref()
        .and_then(tilo_ui::color::parse_hex)
        .map(|colour| {
            let rgba = colour.to_rgba8();
            [rgba.r, rgba.g, rgba.b]
        });
    let field = crate::wand::coverage(
        &crate::wand::Picture {
            rgba: &image.rgba,
            width: image.width as usize,
            height: image.height as usize,
        },
        &crate::wand::Wand {
            seed,
            colour,
            tolerance: find.tolerance.unwrap_or(0.15).clamp(0.0, 1.0),
            everywhere: find.everywhere.unwrap_or(false),
            allowed: None,
            pixels_per_unit: 1.0,
        },
    );
    let region = crate::region::Coverage {
        width: image.width as usize,
        height: image.height as usize,
        cells: &field,
    }
    .traced(tilo_ui::kurbo::Affine::IDENTITY);
    let mut path = region.to_path();
    if let Some([x, y, width, height]) = find.frame {
        let scale = tilo_ui::kurbo::Affine::scale_non_uniform(
            width / f64::from(image.width.max(1)),
            height / f64::from(image.height.max(1)),
        );
        path = tilo_ui::kurbo::Affine::translate((x, y)) * scale * path;
    }
    Ok(path.to_svg())
}

/// A look at `art` as `request` asks, as PNG or JPEG bytes and the info JSON.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn look(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<(Vec<u8>, String), String> {
    let request: Look = serde_json::from_str(request)
        .map_err(|error| format!("a look's fields cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let (picture, info) = view_ops::look(&mut renderer, &mut text, art, &request)?;
        let info = serde_json::to_string(&info)
            .map_err(|error| format!("the look's answer cannot be written: {error}"))?;
        Ok((picture.bytes, info))
    })
}

/// The area `request` names in `art` as the window shows it, as SVG path data: the magic wand at
/// a point or a colour, a picture's subject, or the selection's edge refined. The soft edge a
/// refine keeps is not carried: the records this bridge serves hold the area's outline.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn find_area(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<String, String> {
    let select: crate::resource::FindArea = serde_json::from_str(request)
        .map_err(|error| format!("what to find cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let prepared = crate::subject::prepare(&select, &mut renderer, &mut text, art)?;
        let (region, _matte, _found) = prepared.finish()?;
        Ok(region.to_path().to_svg())
    })
}

/// Combines the marks `request` names into one path, as the path the bottom mark becomes: its
/// outline in artwork units, and the fill it keeps.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn combine(art: &Artwork, request: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ask {
        /// The marks to combine, the bottom one first.
        ids: Vec<String>,
        /// `union`, `subtract`, `intersect` or `exclude`.
        op: String,
    }
    let ask: Ask = serde_json::from_str(request)
        .map_err(|error| format!("what to combine cannot be read: {error}"))?;
    let mut doc = art.clone();
    let combine = crate::selection::Combine {
        ids: ask.ids.clone(),
        op: ask.op,
    };
    combine
        .apply(&mut doc, art)
        .map_err(|refusal| refusal.to_string())?;
    // The combined mark takes the bottom one's place and id.
    let mark = doc
        .marks
        .iter()
        .find(|mark| ask.ids.contains(&mark.id))
        .ok_or_else(|| "the combined shape is gone".to_owned())?;
    let crate::model::Shape::Path { d } = &mark.shape else {
        return Err("the combined shape is not a path".to_owned());
    };
    Ok(serde_json::json!({
        "d": d,
        "fill": mark.style.fill.clone().unwrap_or_default(),
        "stroke": mark.style.stroke.clone().unwrap_or_default(),
        "stroke_width": mark.style.stroke_width,
    })
    .to_string())
}

/// How the marks `request` names put down ink, as the eye sees it: each one's ink box, ink area,
/// centre of mass and its offset from the box's middle.
///
/// # Errors
///
/// The engine's refusal, in words a person reads, or that this device cannot render the picture
/// the measurement needs.
pub fn measure(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ask {
        /// Marks or groups to measure.
        ids: Vec<String>,
    }
    let ask: Ask = serde_json::from_str(request)
        .map_err(|error| format!("what to measure cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let measured = crate::view_ops::measure(&mut renderer, &mut text, art, &ask.ids)?.now()?;
        serde_json::to_string(&measured)
            .map_err(|error| format!("the measurement cannot be written: {error}"))
    })
}

/// `art` beside, over or against its reference picture, as `request` asks: the picture and how
/// far apart they are.
///
/// # Errors
///
/// The engine's refusal, in words a person reads, or that this device cannot render the picture
/// the comparison needs.
pub fn compare(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<(Vec<u8>, String), String> {
    let request: Compare = serde_json::from_str(request)
        .map_err(|error| format!("a comparison's fields cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let (picture, info) =
            crate::view_ops::compare(&mut renderer, &mut text, art, &request)?.now()?;
        let info = serde_json::to_string(&info)
            .map_err(|error| format!("the comparison's answer cannot be written: {error}"))?;
        Ok((picture.bytes, info))
    })
}

/// `art` at full size as `request` asks, as PNG, JPEG or WebP bytes and the info JSON.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn export(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<(Vec<u8>, String), String> {
    let request: Export = serde_json::from_str(request)
        .map_err(|error| format!("an export's fields cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let (picture, info) = view_ops::export(&mut renderer, &mut text, art, &request)?;
        let info = serde_json::to_string(&info)
            .map_err(|error| format!("the export's answer cannot be written: {error}"))?;
        Ok((picture.bytes, info))
    })
}

/// `art`, or the marks `request` names, as an SVG document.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn svg(art: &Artwork, request: &str) -> Result<String, String> {
    let request: ExportSvg = serde_json::from_str(request)
        .map_err(|error| format!("an SVG export's fields cannot be read: {error}"))?;
    let ground = request.ids.as_ref().is_none_or(Vec::is_empty);
    Ok(svg::export(
        art,
        ground,
        |_| None,
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    ))
}

/// An SVG document taken in as marks, as `request` places them: the drawing's JSON.
///
/// # Errors
///
/// Why the SVG cannot be read or does not fit.
pub fn import_svg(markup: &str, request: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Place {
        layer: Option<String>,
        group: Option<String>,
        fit: Option<[f64; 4]>,
    }
    let place: Place = serde_json::from_str(request)
        .map_err(|error| format!("an SVG import's fields cannot be read: {error}"))?;
    let draw = svg::import(&ImportSvg {
        svg: markup.to_owned(),
        layer: place.layer,
        group: place.group,
        fit: place.fit,
    })?;
    serde_json::to_string(&draw)
        .map_err(|error| format!("the imported marks cannot be written: {error}"))
}

/// `art` saved as a `.artwork` package, with `pictures` as the bytes it carries.
///
/// # Errors
///
/// A picture the artwork shows that `pictures` does not hold, or a pack that fails.
pub fn save(art: &Artwork, pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let store = ContentStore::memory();
    for (address, bytes) in pictures {
        if let Some(reference) = reference(address) {
            let _ = store.put_as(reference, bytes);
        }
    }
    let file = ArtworkFile::of(art);
    let (bytes, missing) = pack(&file, &store)?;
    if !missing.is_empty() {
        return Err(format!(
            "the artwork shows pictures this device does not hold: {}",
            missing.join(", ")
        ));
    }
    Ok(bytes)
}

/// A `.artwork` package opened: the artwork's JSON and the pictures it carried.
///
/// # Errors
///
/// Why the file is no artwork, or cannot be read.
pub fn open(archive: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let store = ContentStore::memory();
    let file = unpack(archive, &store)?;
    let art = artwork_of(&file)?;
    let mut pictures = Vec::new();
    for address in file.pictures() {
        if let Some(reference) = reference(address)
            && let Some(bytes) = store.read(reference)
        {
            pictures.push((address.to_owned(), bytes));
        }
    }
    let json = serde_json::to_string(&ArtworkFile::of(&art))
        .map_err(|error| format!("the artwork cannot be written: {error}"))?;
    Ok((json, pictures))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The records a package keeps, as its JSON writes them.
    fn records() -> String {
        serde_json::json!({
            "title": "A study",
            "width": 800.0,
            "height": 600.0,
            "ground_texture": "linen",
            "ground_colour": "#ffffff",
            "layers": [{
                "name": "Base",
                "visible": true,
            }, {
                "name": "Ink",
                "medium": "",
                "opacity": 0.5,
                "visible": true,
                "locked": true,
                "texture": 0.25,
                "group": "icons",
                "mask_of": "Base",
                "base": 0.4,
                "invert": true,
                "clip": true,
                "unlinked": true,
            }],
            "media": [{
                "id": "studio-oil",
                "label": "Studio oil",
                "engine": "oil",
                "params": { "load": 0.3 },
                "palette": [{ "name": "burnt-sienna", "colour": "#8a3324" }],
            }],
            "reference": {
                "image": "tilo-blob://bbbb",
                "frame": [1.0, 2.0, 30.0, 20.0],
                "opacity": 0.4,
                "visible": true,
            },
            "marks": [
                {
                    "layer": { "name": "Ink" },
                    "kind": "stroke",
                    "wet": true,
                    "params": { "flow": 0.5 },
                    "group": "icons",
                    "points": [1.0, 2.0, 0.5, 3.0, 4.0, 0.6],
                    "size": 6.0,
                    "colour": "#d24b3a",
                    "brush": "pen",
                    "opacity": 0.8,
                },
                {
                    "layer": { "name": "Ink" },
                    "kind": "path",
                    "d": "M 0 0 L 10 10 Z",
                    "colour": "#2f7fb8",
                },
                {
                    "layer": { "name": "Ink" },
                    "kind": "image",
                    "name": "Dawn.png",
                    "src": "tilo-blob://aaaa",
                    "x": 10.0,
                    "y": 20.0,
                    "width": 300.0,
                    "height": 200.0,
                    "blend": "multiply",
                    "crop_x": 0.1,
                    "crop_y": 0.2,
                    "crop_width": 0.5,
                    "crop_height": 0.6,
                    "straighten": 4.0,
                    "flip_h": true,
                    "exposure": 1.5,
                    "contrast": 0.25,
                    "grayscale": true,
                },
            ],
            "selection": { "d": "M 0 0 L 5 5 Z", "feather": 2.0 },
        })
        .to_string()
    }

    #[test]
    fn the_records_a_package_keeps_carry_its_paint_and_its_layers() {
        let art = artwork_of_records(&records()).expect("the records are an artwork");
        assert_eq!(art.ground.texture, "linen");
        let mut layers = art.layers.iter();
        let base = layers.next().expect("the layer under");
        assert_eq!(base.name, "Base");
        let layer = layers.next().expect("the mask");
        assert_eq!(layer.name, "Ink");
        assert!((layer.opacity - 0.5).abs() < f64::EPSILON);
        assert!((layer.texture - 0.25).abs() < f64::EPSILON);
        assert!(layer.locked, "a held-still layer stays held still");
        assert_eq!(layer.group, "icons");
        assert_eq!(layer.mask_of, base.id, "the mask names the layer it masks");
        assert!((layer.base - 0.4).abs() < f64::EPSILON);
        assert!(layer.invert && layer.clip && layer.unlinked);

        let mut marks = art.marks.iter();
        let stroke = marks.next().expect("the stroke");
        assert_eq!(&stroke.layer, &layer.id, "the mark lands on its layer");
        assert_eq!(stroke.style.brush.as_deref(), Some("pen"));
        assert_eq!(
            stroke.style.stroke.as_deref(),
            Some("#d24b3a"),
            "a stroke paints in the ink the person chose"
        );
        assert_eq!(stroke.style.opacity, Some(0.8));
        assert_eq!(stroke.group, "icons", "the mark keeps its folder");
        assert!(stroke.style.wet, "the mark meets the wet paint");
        assert_eq!(stroke.style.params.get("flow"), Some(&0.5));
        let path = marks.next().expect("the shape");
        assert_eq!(path.style.fill.as_deref(), Some("#2f7fb8"));
        let picture = marks.next().expect("the picture");
        match &picture.shape {
            Shape::Image {
                src,
                x,
                y,
                width,
                height,
                crop,
                straighten,
                ..
            } => {
                assert_eq!(src, "tilo-blob://aaaa");
                assert!((x - 10.0).abs() < f64::EPSILON);
                assert!((y - 20.0).abs() < f64::EPSILON);
                assert!((width - 300.0).abs() < f64::EPSILON);
                assert!((height - 200.0).abs() < f64::EPSILON);
                assert_eq!(picture.style.blend.as_deref(), Some("multiply"));
                assert_eq!(*crop, Some([0.1, 0.2, 0.5, 0.6]));
                assert_eq!(*straighten, Some(4.0));
                assert_eq!(
                    picture.style.adjust.as_ref().and_then(|a| a.exposure),
                    Some(1.5)
                );
                assert_eq!(
                    picture.style.adjust.as_ref().and_then(|a| a.grayscale),
                    Some(true)
                );
                let flip = picture.transform.expect("a flipped picture");
                assert!(flip[0] < 0.0, "the picture is mirrored across: {flip:?}");
            }
            other => panic!("an image, not {other:?}"),
        }

        let medium = art.media.get("studio-oil").expect("the saved medium");
        assert_eq!(medium.label, "Studio oil");
        assert_eq!(medium.engine, "oil");
        assert_eq!(medium.params.get("load"), Some(&0.3));
        assert_eq!(medium.palette.len(), 1, "the medium keeps its pigments");
        assert_eq!(medium.palette[0].name, "burnt-sienna");

        let reference = art.reference.expect("the reference picture");
        assert_eq!(reference.image, "tilo-blob://bbbb");
        assert_eq!(reference.frame, Some([1.0, 2.0, 30.0, 20.0]));
        assert!((reference.opacity - 0.4).abs() < f64::EPSILON);

        let selection = art.selection.expect("the selection is kept");
        assert_eq!(selection.d, "M 0 0 L 5 5 Z");
    }
}
