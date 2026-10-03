//! What a caller with no screen asks of the artwork: a look at it, a list of
//! what is on it, the media it can use, a full-size export.
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
use crate::model::{Artwork, Shape};
use crate::render::{Options, Renderer};
use crate::resource::{Compare, Export, Inspect, Look};

/// The default longest side of a look, in pixels.
pub const LOOK_SIZE: u32 = 1024;
/// The largest look.
pub const MAX_LOOK_SIZE: u32 = 1536;
/// The most marks `inspect` lists.
pub const MAX_LISTED: usize = 400;

/// A rendered picture.
pub struct Picture {
    pub png: Vec<u8>,
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

/// Render `region` of `art` with `pixels_per_unit`, as a PNG.
///
/// # Errors
/// Returns why nothing could be rendered: no GPU adapter, a failed encode.
pub fn render(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    region: Rect,
    pixels_per_unit: f64,
    options: &Options,
) -> Result<Picture, String> {
    let (rgba, width, height) = pixels(region, pixels_per_unit, options.bare, |scene, to| {
        renderer.draw(art, scene, to, text, options);
    })?;
    Ok(Picture {
        png: encode_png(&rgba, width, height)?,
        width,
        height,
    })
}

/// What `paint` draws of `region`, as RGBA: over a neutral grey, or over
/// transparency when `bare`.
///
/// # Errors
/// Returns why nothing could be rendered.
pub fn pixels(
    region: Rect,
    pixels_per_unit: f64,
    bare: bool,
    paint: impl FnOnce(&mut Scene, Affine),
) -> Result<(Vec<u8>, u32, u32), String> {
    let width = side(region.width() * pixels_per_unit);
    let height = side(region.height() * pixels_per_unit);
    let mut scene = Scene::new();
    let transform = Affine::scale(pixels_per_unit) * Affine::translate((-region.x0, -region.y0));
    paint(&mut scene, transform);
    let mut offscreen = ui::offscreen::OffscreenRenderer::try_new(width, height)
        .map_err(|error| format!("no renderer for a {width}x{height} picture: {error}"))?;
    // A neutral backdrop outside the artwork, the same in every theme so
    // two looks at one revision are the same picture.
    let backdrop = if bare {
        Color::TRANSPARENT
    } else {
        Color::from_rgba8(120, 120, 120, 255) // picture backdrop
    };
    Ok((offscreen.render(&scene, backdrop), width, height))
}

/// The cells `compare` scores the picture in, per side.
const COMPARE_CELLS: u32 = 4;

/// The artwork against its reference picture.
///
/// # Errors
/// Returns that there is no reference, an invalid region, mode or size, or
/// a render failure.
pub fn compare(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    compare: &Compare,
) -> Result<(Picture, Value), String> {
    if art.reference.is_none() {
        return Err("there is no reference picture; set-reference first".into());
    }
    let frame = renderer
        .reference_frame(art)
        .ok_or("the reference picture cannot be read")?;
    let region = match compare.region {
        Some(region) => region_rect(art, Some(region))?,
        None => frame,
    };
    let mode = compare.mode.as_deref().unwrap_or("side-by-side");
    if !matches!(mode, "side-by-side" | "overlay" | "difference") {
        return Err(format!(
            "mode {mode:?} must be side-by-side, overlay or difference"
        ));
    }
    let size = compare.size.unwrap_or(640);
    if !(64..=1024).contains(&size) {
        return Err("size must be within 64..=1024".into());
    }
    let scale = f64::from(size) / region.width().max(region.height());
    let (drawn, width, height) = pixels(region, scale, false, |scene, to| {
        renderer.draw(art, scene, to, text, &Options::default());
    })?;
    let (reference, _, _) = pixels(region, scale, true, |scene, to| {
        renderer.draw_reference(art, scene, to);
    })?;
    let ground = crate::render::colour(&art.ground.colour, &[])
        .unwrap_or(Color::WHITE) // document colour
        .to_rgba8();
    let inside = Rect::new(
        (frame.x0 - region.x0) * scale,
        (frame.y0 - region.y0) * scale,
        (frame.x1 - region.x0) * scale,
        (frame.y1 - region.y0) * scale,
    );
    let reference = flatten(&reference, width, inside, [ground.r, ground.g, ground.b]);
    let (score, cells) = difference(&drawn, &reference, width, height);
    let combined = match mode {
        "overlay" => blend_pixels(&drawn, &reference, |a, b, alpha| {
            a * (1.0 - alpha * 0.5) + b * alpha * 0.5
        }),
        "difference" => blend_pixels(&drawn, &reference, |a, b, alpha| (a - b).abs() * alpha),
        _ => beside(&drawn, &reference, width, height),
    };
    let out_width = if mode == "side-by-side" {
        width.saturating_mul(2).saturating_add(GUTTER)
    } else {
        width
    };
    let cell_w = region.width() / f64::from(COMPARE_CELLS);
    let cell_h = region.height() / f64::from(COMPARE_CELLS);
    let mut worst: Vec<(u32, u32, f64)> = cells;
    worst.sort_by(|a, b| b.2.total_cmp(&a.2));
    let worst: Vec<Value> = worst
        .iter()
        .take(4)
        .map(|(column, row, score)| {
            json!({
                "region": [
                    round(region.x0 + f64::from(*column) * cell_w),
                    round(region.y0 + f64::from(*row) * cell_h),
                    round(cell_w),
                    round(cell_h),
                ],
                "difference": (score * 1000.0).round() / 1000.0,
            })
        })
        .collect();
    let info = json!({
        "mode": mode,
        "width": out_width,
        "height": height,
        "region": [region.x0, region.y0, region.width(), region.height()],
        "difference": (score * 1000.0).round() / 1000.0,
        "worst_regions": worst,
        "reading": "difference is the mean colour distance 0..1 within the reference's frame, its transparent parts read as the ground; lower is closer. side-by-side shows the artwork left, the reference right.",
    });
    Ok((
        Picture {
            png: encode_png(&combined, out_width, height)?,
            width: out_width,
            height,
        },
        info,
    ))
}

/// The gap between the two pictures side by side, in pixels.
const GUTTER: u32 = 8;

/// The reference as the artwork should look within its frame: opaque over
/// the ground, so paint where the reference is empty counts against a
/// match. Outside the frame it stays transparent and is not compared.
fn flatten(reference: &[u8], width: u32, inside: Rect, ground: [u8; 3]) -> Vec<u8> {
    let width = usize::try_from(width.max(1)).unwrap_or(1);
    let mut out = Vec::with_capacity(reference.len());
    for (index, pixel) in reference.chunks_exact(4).enumerate() {
        let x = index % width;
        let y = index / width;
        let centre = ui::vello::kurbo::Point::new(count(x) + 0.5, count(y) + 0.5);
        if !inside.contains(centre) {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let alpha = f64::from(pixel.get(3).copied().unwrap_or(0)) / 255.0;
        for (channel, under) in ground.iter().enumerate() {
            let over = f64::from(pixel.get(channel).copied().unwrap_or(0));
            out.push(byte(over * alpha + f64::from(*under) * (1.0 - alpha)));
        }
        out.push(255);
    }
    out
}

fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The mean colour distance within the reference's frame, overall and per
/// cell of a `COMPARE_CELLS` grid.
fn difference(
    drawn: &[u8],
    reference: &[u8],
    width: u32,
    height: u32,
) -> (f64, Vec<(u32, u32, f64)>) {
    let cells = usize::try_from(COMPARE_CELLS * COMPARE_CELLS).unwrap_or(16);
    let mut sums = vec![(0.0_f64, 0.0_f64); cells];
    let (mut total, mut weight) = (0.0, 0.0);
    for (index, (a, b)) in drawn
        .chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .enumerate()
    {
        let alpha = f64::from(b.get(3).copied().unwrap_or(0)) / 255.0;
        if alpha <= 0.0 {
            continue;
        }
        let distance = (0..3)
            .map(|channel| {
                (f64::from(a.get(channel).copied().unwrap_or(0))
                    - f64::from(b.get(channel).copied().unwrap_or(0)))
                .abs()
            })
            .sum::<f64>()
            / (3.0 * 255.0);
        total += distance * alpha;
        weight += alpha;
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let (x, y) = (index % width.max(1), index / width.max(1));
        let cell =
            (y * COMPARE_CELLS / height.max(1)) * COMPARE_CELLS + x * COMPARE_CELLS / width.max(1);
        if let Some(sum) = usize::try_from(cell)
            .ok()
            .and_then(|cell| sums.get_mut(cell))
        {
            sum.0 += distance * alpha;
            sum.1 += alpha;
        }
    }
    let per_cell = (0..COMPARE_CELLS * COMPARE_CELLS)
        .zip(sums)
        .filter(|(_, (_, weight))| *weight > 0.0)
        .map(|(cell, (sum, weight))| (cell % COMPARE_CELLS, cell / COMPARE_CELLS, sum / weight))
        .collect();
    (if weight > 0.0 { total / weight } else { 1.0 }, per_cell)
}

/// `drawn` and `reference` combined channel by channel by `mix(a, b,
/// reference alpha)`.
fn blend_pixels(drawn: &[u8], reference: &[u8], mix: impl Fn(f64, f64, f64) -> f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(drawn.len());
    for (a, b) in drawn.chunks_exact(4).zip(reference.chunks_exact(4)) {
        let alpha = f64::from(b.get(3).copied().unwrap_or(0)) / 255.0;
        for channel in 0..3 {
            let value = mix(
                f64::from(a.get(channel).copied().unwrap_or(0)),
                f64::from(b.get(channel).copied().unwrap_or(0)),
                alpha,
            );
            out.push(byte(value));
        }
        out.push(255);
    }
    out
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to a byte's range before the cast"
)]
fn byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The two pictures side by side with a gutter, the reference over grey.
fn beside(drawn: &[u8], reference: &[u8], width: u32, height: u32) -> Vec<u8> {
    let grey = blend_pixels(&vec![120; reference.len()], reference, |a, b, alpha| {
        a * (1.0 - alpha) + b * alpha
    });
    let row = usize::try_from(width).unwrap_or(0) * 4;
    let gutter = vec![120_u8; usize::try_from(GUTTER).unwrap_or(0) * 4];
    let mut out = Vec::new();
    for y in 0..usize::try_from(height).unwrap_or(0) {
        let span = y * row..(y + 1) * row;
        out.extend_from_slice(drawn.get(span.clone()).unwrap_or_default());
        out.extend_from_slice(&gutter);
        out.extend_from_slice(grey.get(span).unwrap_or_default());
    }
    out
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
    let scale = f64::from(size) / region.width().max(region.height());
    let options = Options {
        layers: look.layers.clone(),
        grid: look.grid.unwrap_or(false),
        view: Some(region),
        reference: look.reference.unwrap_or_else(|| {
            art.reference
                .as_ref()
                .is_some_and(|reference| reference.visible)
        }),
        bare: false,
    };
    let picture = render(renderer, text, art, region, scale, &options)?;
    let info = json!({
        "width": picture.width,
        "height": picture.height,
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
        art.bounds(),
        scale,
        &Options::default(),
    )?;
    let info = json!({"width": picture.width, "height": picture.height, "format": "png"});
    Ok((picture, info))
}

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
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
        .filter(|mark| {
            region.is_none_or(|region| art.visible_bounds(mark).intersect(region).area() > 0.0)
        })
        .collect();
    let marks: Vec<Value> = matching
        .iter()
        .rev()
        .take(MAX_LISTED)
        .rev()
        .map(|mark| {
            let bounds = art.visible_bounds(mark);
            let mut entry = json!({
                "id": mark.id,
                "kind": mark.shape.kind(),
                "layer": mark.layer,
                "bounds": [round(bounds.x0), round(bounds.y0), round(bounds.width()), round(bounds.height())],
            });
            if let Value::Object(fields) = &mut entry {
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
                if !mark.group.is_empty() {
                    fields.insert("group".into(), mark.group.clone().into());
                }
                if !mark.clip.is_empty() {
                    fields.insert("clip".into(), mark.clip.clone().into());
                }
                if inspect.full == Some(true) {
                    fields.insert("shape".into(), json!(mark.shape));
                    fields.insert("style".into(), json!(mark.style));
                    if let Some(transform) = mark.transform {
                        fields.insert("transform".into(), json!(transform));
                    }
                }
            }
            entry
        })
        .collect();
    let layers: Vec<Value> = art
        .layers
        .iter()
        .rev()
        .map(|layer| {
            json!({
                "id": layer.id,
                "name": layer.name,
                "medium": if layer.medium.is_empty() { "plain" } else { layer.medium.as_str() },
                "opacity": layer.opacity,
                "visible": layer.visible,
                "marks": art.marks_of(&layer.id).count(),
            })
        })
        .collect();
    Ok(json!({
        "artwork": {
            "title": art.title,
            "width": art.width,
            "height": art.height,
            "texture": art.ground.texture,
            "ground_colour": art.ground.colour,
        },
        "layers_top_first": layers,
        "groups": art.groups(),
        "marks_bottom_first": marks,
        "listed": marks.len(),
        "matching": matching.len(),
    }))
}

/// The media layers can use.
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
                    "min": spec.min,
                    "max": spec.max,
                    "default": spec.default,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let media: Vec<Value> = media::available(art)
        .iter()
        .map(|medium| {
            json!({
                "id": medium.id,
                "label": medium.label,
                "engine": medium.engine,
                "description": medium.description,
                "ground": medium.ground,
                "params": medium.params,
                "saved_in_artwork": art.media.contains_key(&medium.id),
                "pigments": medium.palette.iter().map(|pigment| format!("{} {}", pigment.name, pigment.colour)).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "media": media,
        "engines": engines,
        "textures": ["smooth", "paper", "cold-press", "rough", "linen"],
        "plain_layers": "a layer with no medium draws crisp vector art; pigment:<name> resolves against every palette there",
    })
}

/// The versions of the document, oldest first, as an agent reads them.
pub fn history(doc: &mut ui::automerge::AutoCommit, view: &crate::resource::HistoryView) -> Value {
    let last = usize::try_from(view.last.unwrap_or(100).clamp(1, 500)).unwrap_or(100);
    let versions = ui::tilo_resource::history::versions(doc);
    let total = versions.len();
    let listed: Vec<Value> = versions
        .iter()
        .skip(total.saturating_sub(last))
        .map(|version| {
            let context = version.context.as_ref();
            json!({
                "change": version.hash.to_string().get(..12).unwrap_or_default(),
                "action": context.map(|context| context.action_label.clone()).or_else(|| version.summary()),
                "group": context.and_then(|context| context.group.as_ref()).map(|group| group.label.clone()),
                "time_ms": version.time_ms,
            })
        })
        .collect();
    json!({"versions": listed, "total": total})
}
