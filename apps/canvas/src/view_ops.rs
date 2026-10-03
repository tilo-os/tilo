//! What a caller with no screen asks of the artwork: a look at it, a list of
//! what is on it, the media it can use, a full-size export, what changed
//! between two versions.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use serde_json::{Value, json};
use ui::TextRenderer;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Rect};
use ui::vello::peniko::Color;

use crate::media;
use crate::model::Mark;
use crate::model::{Artwork, Shape};
use crate::render::{Options, Renderer};
use crate::resource::{Export, Inspect, Look};

/// The default longest side of a look, in pixels: enough to judge a
/// composition; a region look shows detail. A model pays for every pixel.
pub const LOOK_SIZE: u32 = 768;
/// The largest look.
pub const MAX_LOOK_SIZE: u32 = 1536;
/// The longest side of each half of a diff's picture.
pub const DIFF_LOOK_SIZE: u32 = 640;
/// The gap between a diff picture's halves, in pixels.
const DIFF_GAP: u32 = 8;
/// The neutral backdrop outside the artwork, the same in every theme so
/// two looks at one revision are the same picture.
const BACKDROP: [u8; 4] = [120, 120, 120, 255];
/// The most marks `inspect` lists, and how many when the caller names no
/// limit.
pub const MAX_LISTED: usize = 400;
pub const LISTED: usize = 100;
/// JPEG quality of a look: paint and edges read as they are, at a fraction
/// of a PNG's bytes.
const JPEG_QUALITY: u8 = 85;

/// A rendered picture, encoded.
pub struct Picture {
    pub bytes: Vec<u8>,
    /// `png` or `jpeg`.
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
}

fn region_rect(art: &Artwork, region: Option<[f64; 4]>) -> Result<Rect, String> {
    let Some([x, y, width, height]) = region else {
        return Ok(art.bounds());
    };
    if ![x, y, width, height].iter().all(|value| value.is_finite()) || width <= 0.0 || height <= 0.0
    {
        return Err("region is [x, y, width, height] with a positive width and height".into());
    }
    let rect = Rect::new(x, y, x + width, y + height);
    if rect.intersect(art.bounds()).area() <= 0.0 {
        return Err(format!(
            "region {region:?} is outside the artwork (0, 0, {}, {})",
            art.width, art.height
        ));
    }
    Ok(rect)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "picture sides are clamped to 1..=8192 before the cast"
)]
fn side(value: f64) -> u32 {
    value.round().clamp(1.0, 8192.0) as u32
}

/// Render `region` of `art` with `pixels_per_unit`, as a PNG, or as a
/// JPEG when `jpeg`.
///
/// # Errors
/// Returns why nothing could be rendered: no GPU adapter, a failed encode.
pub fn render(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    (region, pixels_per_unit): (Rect, f64),
    options: &Options,
    jpeg: bool,
) -> Result<Picture, String> {
    let (rgba, width, height) = render_rgba(renderer, text, art, region, pixels_per_unit, options)?;
    encode(&rgba, (width, height), jpeg)
}

fn encode(rgba: &[u8], (width, height): (u32, u32), jpeg: bool) -> Result<Picture, String> {
    let (bytes, format) = if jpeg {
        (encode_jpeg(rgba, width, height)?, "jpeg")
    } else {
        (encode_png(rgba, width, height)?, "png")
    };
    Ok(Picture {
        bytes,
        format,
        width,
        height,
    })
}

fn render_rgba(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    region: Rect,
    pixels_per_unit: f64,
    options: &Options,
) -> Result<(Vec<u8>, u32, u32), String> {
    let width = side(region.width() * pixels_per_unit);
    let height = side(region.height() * pixels_per_unit);
    let mut scene = Scene::new();
    let transform = Affine::scale(pixels_per_unit) * Affine::translate((-region.x0, -region.y0));
    renderer.draw(art, &mut scene, transform, text, options);
    let mut offscreen = ui::offscreen::OffscreenRenderer::try_new(width, height)
        .map_err(|error| format!("no renderer for a {width}x{height} picture: {error}"))?;
    let [r, g, b, a] = BACKDROP;
    let rgba = offscreen.render(&scene, Color::from_rgba8(r, g, b, a));
    Ok((rgba, width, height))
}

/// Opaque RGBA8 as JPEG bytes; alpha is dropped.
///
/// # Errors
/// Returns the encoder's error.
pub fn encode_jpeg(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|pixel| pixel.iter().take(3).copied())
        .collect();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|error| error.to_string())?;
    Ok(jpeg)
}

/// RGBA8 as PNG bytes.
///
/// # Errors
/// Returns the encoder's error.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgba))
        .map_err(|error| error.to_string())?;
    Ok(png)
}

/// A look at the artwork: the picture and what it shows.
///
/// # Errors
/// Returns an invalid region or size, or a render failure.
pub fn look(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    look: &Look,
) -> Result<(Picture, Value), String> {
    let region = region_rect(art, look.region)?;
    let size = look.size.unwrap_or(LOOK_SIZE);
    if !(64..=MAX_LOOK_SIZE).contains(&size) {
        return Err(format!("size must be within 64..={MAX_LOOK_SIZE}"));
    }
    if let Some(unknown) = look
        .layers
        .iter()
        .flatten()
        .find(|id| art.layer(id).is_none())
    {
        return Err(format!("there is no layer {unknown:?}"));
    }
    let jpeg = match look.format.as_deref() {
        None | Some("jpeg") => true,
        Some("png") => false,
        Some(other) => return Err(format!("format {other:?} is jpeg or png")),
    };
    let scale = f64::from(size) / region.width().max(region.height());
    let options = Options {
        layers: look.layers.clone(),
        grid: look.grid.unwrap_or(false),
        view: Some(region),
    };
    let picture = render(renderer, text, art, (region, scale), &options, jpeg)?;
    let info = json!({
        "width": picture.width,
        "height": picture.height,
        "format": picture.format,
        "region": [region.x0, region.y0, region.width(), region.height()],
        "pixels_per_unit": round(scale),
        "marks": art.marks.len(),
    });
    Ok((picture, info))
}

/// The artwork at full size.
///
/// # Errors
/// Returns an invalid scale or a render failure.
pub fn export(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    export: &Export,
) -> Result<(Picture, Value), String> {
    let scale = export.scale.unwrap_or(1.0);
    if !(0.25..=4.0).contains(&scale) || art.width.max(art.height) * scale > 8192.0 {
        return Err(
            "scale must be within 0.25..=4 and the picture at most 8192 pixels wide".into(),
        );
    }
    let picture = render(
        renderer,
        text,
        art,
        (art.bounds(), scale),
        &Options::default(),
        false,
    )?;
    let info = json!({"width": picture.width, "height": picture.height, "format": "png"});
    Ok((picture, info))
}

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// A coordinate to the artwork unit: a tenth more is noise to a caller and
/// bytes in every row.
fn whole(value: f64) -> f64 {
    value.round()
}

/// One mark as `inspect` lists it; its layer only when the caller did not
/// name one.
fn mark_entry(mark: &crate::model::Mark, named_layer: bool) -> Value {
    let bounds = mark.bounds();
    let mut entry = json!({
        "id": mark.id,
        "kind": mark.shape.kind(),
        "bounds": [whole(bounds.x0), whole(bounds.y0), whole(bounds.width()), whole(bounds.height())],
    });
    if let Value::Object(fields) = &mut entry {
        if !named_layer {
            fields.insert("layer".into(), mark.layer.clone().into());
        }
        if !mark.name.is_empty() {
            fields.insert("name".into(), mark.name.clone().into());
        }
        if let Some(fill) = &mark.style.fill {
            fields.insert("fill".into(), fill.clone().into());
        }
        if let Some(stroke) = &mark.style.stroke {
            fields.insert("stroke".into(), stroke.clone().into());
        }
        if let Shape::Text { text, .. } = &mark.shape {
            fields.insert("text".into(), text.clone().into());
        }
        if let Shape::Stroke { size, .. } = &mark.shape {
            fields.insert("size".into(), round(*size).into());
        }
    }
    entry
}

/// The layers and marks, as an agent reads them.
///
/// # Errors
/// Returns an invalid region or an unknown layer.
pub fn inspect(art: &Artwork, inspect: &Inspect) -> Result<Value, String> {
    let region = inspect
        .region
        .map(|region| region_rect(art, Some(region)))
        .transpose()?;
    if let Some(layer) = &inspect.layer
        && art.layer(layer).is_none()
    {
        return Err(format!("there is no layer {layer:?}"));
    }
    let matching: Vec<_> = art
        .marks
        .iter()
        .filter(|mark| {
            inspect
                .layer
                .as_ref()
                .is_none_or(|layer| mark.layer == *layer)
        })
        .filter(|mark| region.is_none_or(|region| mark.bounds().intersect(region).area() > 0.0))
        .collect();
    let limit = inspect
        .limit
        .map_or(LISTED, |limit| usize::try_from(limit).unwrap_or(MAX_LISTED));
    if !(1..=MAX_LISTED).contains(&limit) {
        return Err(format!("limit must be within 1..={MAX_LISTED}"));
    }
    let marks: Vec<Value> = matching
        .iter()
        .rev()
        .take(limit)
        .rev()
        .map(|mark| mark_entry(mark, inspect.layer.is_some()))
        .collect();
    let layers: Vec<Value> = art
        .layers
        .iter()
        .rev()
        .map(|layer| {
            let mut entry = json!({
                "id": layer.id,
                "name": layer.name,
                "medium": if layer.medium.is_empty() { "plain" } else { layer.medium.as_str() },
                "marks": art.marks_of(&layer.id).count(),
            });
            if let Value::Object(fields) = &mut entry {
                if layer.opacity < 1.0 {
                    fields.insert("opacity".into(), layer.opacity.into());
                }
                if !layer.visible {
                    fields.insert("visible".into(), false.into());
                }
            }
            entry
        })
        .collect();
    let mut answer = json!({
        "artwork": {
            "title": art.title,
            "width": art.width,
            "height": art.height,
            "texture": art.ground.texture,
            "ground_colour": art.ground.colour,
        },
        "layers_top_first": layers,
        "marks_bottom_first": marks,
        "listed": marks.len(),
        "matching": matching.len(),
    });
    if marks.len() < matching.len()
        && let Value::Object(fields) = &mut answer
    {
        fields.insert(
            "more".into(),
            format!(
                "the newest {} of {} marks; a layer, region or limit (at most {MAX_LISTED}) lists others",
                marks.len(),
                matching.len()
            )
            .into(),
        );
    }
    Ok(answer)
}

/// A medium's number as an agent reads it: two decimals say what an f32
/// slider value means; its float noise says nothing.
fn hundredths(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// The media layers can use. Each pigment palette is listed once, under
/// the first medium that uses it, and named by every medium that does.
pub fn media(art: &Artwork) -> Value {
    let engines = tilo_wet_media::Engines::builtin();
    let engines: Vec<Value> = engines
        .iter()
        .map(|engine| {
            json!({
                "id": engine.id(),
                "label": engine.label(),
                "description": engine.description(),
                "parameters": engine.parameters().iter().map(|spec| json!({
                    "id": spec.id,
                    "description": spec.description,
                    "min": hundredths(f64::from(spec.min)),
                    "max": hundredths(f64::from(spec.max)),
                    "default": hundredths(f64::from(spec.default)),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut palettes = serde_json::Map::new();
    let mut seen: Vec<(Vec<String>, String)> = Vec::new();
    let media: Vec<Value> = media::available(art)
        .iter()
        .map(|medium| {
            let pigments: Vec<String> = medium
                .palette
                .iter()
                .map(|pigment| format!("{} {}", pigment.name, pigment.colour))
                .collect();
            let palette = if let Some((_, id)) = seen.iter().find(|(listed, _)| *listed == pigments)
            {
                id.clone()
            } else {
                palettes.insert(medium.id.clone(), pigments.clone().into());
                seen.push((pigments, medium.id.clone()));
                medium.id.clone()
            };
            let params: serde_json::Map<String, Value> = medium
                .params
                .iter()
                .map(|(name, value)| (name.clone(), hundredths(*value).into()))
                .collect();
            json!({
                "id": medium.id,
                "label": medium.label,
                "engine": medium.engine,
                "description": medium.description,
                "ground": medium.ground,
                "params": params,
                "saved_in_artwork": art.media.contains_key(&medium.id),
                "palette": palette,
            })
        })
        .collect();
    json!({
        "media": media,
        "palettes": palettes,
        "engines": engines,
        "textures": ["smooth", "paper", "cold-press", "rough", "linen"],
        "plain_layers": "a layer with no medium draws crisp vector art; pigment:<name> resolves against every palette there",
    })
}

/// Two versions of the artwork and their names.
pub struct Versions {
    pub before: Artwork,
    pub after: Artwork,
    pub from: String,
    pub to: String,
}

/// Ids added, removed and changed between two versions.
#[derive(Default)]
struct Changes {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
}

impl Changes {
    fn of<T: PartialEq>(before: &[T], after: &[T], id: fn(&T) -> &str) -> Self {
        let mut changes = Self::default();
        for item in after {
            match before.iter().find(|old| id(old) == id(item)) {
                None => changes.added.push(id(item).to_owned()),
                Some(old) if old != item => changes.changed.push(id(item).to_owned()),
                Some(_) => {}
            }
        }
        for item in before {
            if !after.iter().any(|new| id(new) == id(item)) {
                changes.removed.push(id(item).to_owned());
            }
        }
        changes
    }

    fn ids(&self) -> impl Iterator<Item = &String> {
        self.added.iter().chain(&self.removed).chain(&self.changed)
    }

    fn is_empty(&self) -> bool {
        self.ids().next().is_none()
    }

    fn json(&self) -> Value {
        json!({"added": self.added, "removed": self.removed, "changed": self.changed})
    }
}

/// Where `ids` (marks, or every mark of layers) are drawn in either version.
fn drawn(versions: &Versions, marks: &Changes, layers: &Changes) -> Option<Rect> {
    let touched = |mark: &Mark| {
        marks.ids().any(|id| *id == mark.id) || layers.ids().any(|id| *id == mark.layer)
    };
    versions
        .before
        .marks
        .iter()
        .chain(&versions.after.marks)
        .filter(|mark| touched(mark))
        .map(Mark::bounds)
        .reduce(|all, bounds| all.union(bounds))
}

/// What changed between two versions: marks and layers by id, the artwork's
/// own fields, the region the changes are drawn in and, with `look`, a
/// picture of that region before (left) and after (right).
///
/// # Errors
/// Returns a render failure.
pub fn diff(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    versions: &Versions,
    look: bool,
) -> Result<(Option<Picture>, Value), String> {
    let (before, after) = (&versions.before, &versions.after);
    let marks = Changes::of(&before.marks, &after.marks, |mark: &Mark| mark.id.as_str());
    let layers = Changes::of(&before.layers, &after.layers, |layer| layer.id.as_str());
    let mut artwork: Vec<&str> = Vec::new();
    if before.title != after.title {
        artwork.push("title");
    }
    if (before.width, before.height) != (after.width, after.height) {
        artwork.push("size");
    }
    if before.ground != after.ground {
        artwork.push("ground");
    }
    let media: Vec<&String> = before
        .media
        .keys()
        .chain(after.media.keys())
        .filter(|id| before.media.get(*id) != after.media.get(*id))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let whole = artwork.iter().any(|field| *field != "title");
    let region = if whole {
        Some(after.bounds())
    } else {
        drawn(versions, &marks, &layers)
    }
    .map(|region| region.inflate(8.0, 8.0).intersect(after.bounds()))
    .filter(|region| region.area() > 0.0);
    let same = marks.is_empty() && layers.is_empty() && artwork.is_empty() && media.is_empty();
    let info = json!({
        "from": versions.from,
        "to": versions.to,
        "same": same,
        "marks": marks.json(),
        "layers": layers.json(),
        "artwork": artwork,
        "media": media,
        "changed_region": region.map(|region| [round(region.x0), round(region.y0), round(region.width()), round(region.height())]),
    });
    let picture = match region {
        Some(region) if look => Some(side_by_side(renderer, text, versions, region)?),
        _ => None,
    };
    Ok((picture, info))
}

/// `region` of both versions, before left and after right.
fn side_by_side(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    versions: &Versions,
    region: Rect,
) -> Result<Picture, String> {
    let scale = f64::from(DIFF_LOOK_SIZE) / region.width().max(region.height());
    let options = Options {
        view: Some(region),
        ..Options::default()
    };
    let (before, width, height) =
        render_rgba(renderer, text, &versions.before, region, scale, &options)?;
    let (after, _, _) = render_rgba(renderer, text, &versions.after, region, scale, &options)?;
    let total = width.saturating_mul(2).saturating_add(DIFF_GAP);
    let count = |pixels: u32| usize::try_from(pixels).unwrap_or(0);
    let bytes = |pixels: u32| count(pixels).saturating_mul(4);
    let mut rgba = BACKDROP.repeat(count(total).saturating_mul(count(height)));
    let rows = rgba
        .chunks_exact_mut(bytes(total))
        .zip(before.chunks_exact(bytes(width)))
        .zip(after.chunks_exact(bytes(width)));
    for ((out, before), after) in rows {
        let (left, rest) = out.split_at_mut(bytes(width));
        left.copy_from_slice(before);
        rest.split_at_mut(bytes(DIFF_GAP)).1.copy_from_slice(after);
    }
    encode(&rgba, (total, height), true)
}
