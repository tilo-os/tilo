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
use crate::model::{Artwork, Ground, Layer, Mark, Selection, Shape, Style};
use crate::render::Renderer;
use crate::resource::{Export, ExportSvg, Look};
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
        #[serde(default)]
        marks: Vec<RecordMark>,
        #[serde(default)]
        selection: Option<RecordArea>,
    }
    impl Default for RecordLayer {
        fn default() -> Self {
            Self {
                name: String::new(),
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
    struct RecordMark {
        #[serde(default)]
        name: String,
        #[serde(default)]
        layer: RecordLayer,
        #[serde(default)]
        kind: String,
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
            group: String::new(),
            order: order_key(index),
            medium: layer.medium.clone(),
            opacity: layer.opacity.clamp(0.0, 1.0),
            visible: layer.visible,
            texture: layer.texture.clamp(0.0, 1.0),
            locked: layer.locked,
            adjust: None,
            mask_of: String::new(),
            base: 1.0,
            invert: false,
            clip: false,
            unlinked: false,
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
        } else if mark.kind == "image" {
            Shape::Image {
                src: mark.src.clone(),
                x: mark.x,
                y: mark.y,
                width: mark.width,
                height: mark.height,
                crop: None,
                straighten: None,
            }
        } else {
            style.fill = Some(colour);
            Shape::Path { d: mark.d.clone() }
        };
        art.put_mark(Mark {
            id: format!("mark-{index}"),
            layer: slug(&mark.layer.name),
            order: order_key(index),
            name: mark.name.clone(),
            shape,
            style,
            transform: None,
            group: String::new(),
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
    Ok(region.to_path().to_svg())
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
                "name": "Ink",
                "medium": "",
                "opacity": 0.5,
                "visible": true,
                "locked": true,
                "texture": 0.25,
            }],
            "marks": [
                {
                    "layer": { "name": "Ink" },
                    "kind": "stroke",
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
        let layer = art.layers.iter().next().expect("one layer");
        assert_eq!(layer.name, "Ink");
        assert!((layer.opacity - 0.5).abs() < f64::EPSILON);
        assert!((layer.texture - 0.25).abs() < f64::EPSILON);
        assert!(layer.locked, "a held-still layer stays held still");

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
                ..
            } => {
                assert_eq!(src, "tilo-blob://aaaa");
                assert!((x - 10.0).abs() < f64::EPSILON);
                assert!((y - 20.0).abs() < f64::EPSILON);
                assert!((width - 300.0).abs() < f64::EPSILON);
                assert!((height - 200.0).abs() < f64::EPSILON);
            }
            other => panic!("an image, not {other:?}"),
        }

        let selection = art.selection.expect("the selection is kept");
        assert_eq!(selection.d, "M 0 0 L 5 5 Z");
    }
}
