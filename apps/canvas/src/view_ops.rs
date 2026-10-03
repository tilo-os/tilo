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
use crate::model::{Artwork, Layer, Shape};
use crate::render::{Options, Renderer};
use crate::resource::{Compare, Export, Inspect, Look};

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
    pixels(region, pixels_per_unit, options.bare, |scene, to| {
        renderer.draw(art, scene, to, text, options);
    })
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
    let backdrop = if bare {
        Color::TRANSPARENT
    } else {
        let [r, g, b, a] = BACKDROP;
        Color::from_rgba8(r, g, b, a)
    };
    // The renderer for the last size is kept: it has learned how much memory
    // a scene this busy needs, so the next look fits at once.
    let mut kept = OFFSCREEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if kept
        .as_ref()
        .is_none_or(|(size, _)| *size != (width, height))
    {
        let offscreen = ui::offscreen::OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("no renderer for a {width}x{height} picture: {error}"))?;
        *kept = Some(((width, height), offscreen));
    }
    let Some((_, offscreen)) = kept.as_mut() else {
        return Err("no renderer".to_owned());
    };
    let rgba = offscreen.try_render_resilient(&scene, backdrop);
    drop(kept);
    Ok((rgba?, width, height))
}

/// The offscreen renderer of the last picture's size.
static OFFSCREEN: std::sync::Mutex<Option<((u32, u32), ui::offscreen::OffscreenRenderer)>> =
    std::sync::Mutex::new(None);

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
        .unwrap_or(crate::render::BLANK_GROUND)
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
    });
    Ok((encode(&combined, (out_width, height), true)?, info))
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
        reference: look.reference.unwrap_or_else(|| {
            art.reference
                .as_ref()
                .is_some_and(|reference| reference.visible)
        }),
        bare: false,
        wet: None,
        marks: None,
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

/// The longest side, in pixels, a mark is rendered at to measure its ink.
const MEASURE_PIXELS: u32 = 320;
/// The most letters a measured text lists.
const MEASURED_LETTERS: usize = 80;

/// How each mark or group in `ids` puts down ink: its tight ink box, its ink
/// area and the ink's centre of mass, which is where the eye puts its middle.
/// Two icons balance when their ink areas match and each one's `offset`
/// (centre of mass from the middle of its ink box) is the same; a text also
/// lists each letter's ink box.
///
/// # Errors
/// Returns an unknown id or a render failure.
pub fn measure(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    ids: &[String],
) -> Result<Value, String> {
    let mut out = Vec::new();
    for id in ids {
        let members: Vec<&Mark> = match art.mark(id) {
            Some(mark) => vec![mark],
            None => art.members(id).collect(),
        };
        // A text's box is estimated from its characters; its letters say
        // where its ink really is.
        let Some(bounds) = members
            .iter()
            .map(|mark| {
                let estimated = art.visible_bounds(mark);
                crate::render::letters(&mark.shape, text)
                    .into_iter()
                    .map(|letter| {
                        use ui::vello::kurbo::Shape as _;
                        (mark.affine() * letter.path).bounding_box()
                    })
                    .fold(estimated, |all, letter| all.union(letter))
            })
            .reduce(|a, b| a.union(b))
        else {
            return Err(format!("there is no mark or group {id:?} to measure"));
        };
        let region = bounds.inflate(2.0, 2.0);
        let scale =
            (f64::from(MEASURE_PIXELS) / region.width().max(region.height()).max(1.0)).min(8.0);
        let options = Options {
            marks: Some(members.iter().map(|mark| mark.id.clone()).collect()),
            bare: true,
            ..Options::default()
        };
        let (rgba, width, _) = pixels(region, scale, true, |scene, to| {
            renderer.draw(art, scene, to, text, &options);
        })?;
        let width = usize::try_from(width.max(1)).unwrap_or(1);
        let (mut mass, mut mx, mut my) = (0.0, 0.0, 0.0);
        let mut ink: Option<Rect> = None;
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            let alpha = f64::from(pixel.get(3).copied().unwrap_or(0)) / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let (x, y) = (count(index % width) + 0.5, count(index / width) + 0.5);
            mass += alpha;
            mx += alpha * x;
            my += alpha * y;
            if alpha > 0.06 {
                let cell = Rect::new(x - 0.5, y - 0.5, x + 0.5, y + 0.5);
                ink = Some(ink.map_or(cell, |ink| ink.union(cell)));
            }
        }
        let to_units = |x: f64, y: f64| (region.x0 + x / scale, region.y0 + y / scale);
        let mut entry = json!({"id": id});
        if let (Some(ink), Value::Object(fields)) = (ink, &mut entry)
            && mass > 0.0
        {
            let (x0, y0) = to_units(ink.x0, ink.y0);
            let (x1, y1) = to_units(ink.x1, ink.y1);
            let (cx, cy) = to_units(mx / mass, my / mass);
            fields.insert(
                "ink".into(),
                json!([round(x0), round(y0), round(x1 - x0), round(y1 - y0)]),
            );
            fields.insert("ink_area".into(), round(mass / (scale * scale)).into());
            fields.insert("centre".into(), json!([round(cx), round(cy)]));
            fields.insert(
                "offset".into(),
                json!([
                    round(cx - f64::midpoint(x0, x1)),
                    round(cy - f64::midpoint(y0, y1))
                ]),
            );
        }
        if let ([mark], Value::Object(fields)) = (members.as_slice(), &mut entry)
            && matches!(mark.shape, Shape::Text { .. })
        {
            let letters: Vec<Value> = crate::render::letters(&mark.shape, text)
                .into_iter()
                .filter(|letter| !letter.path.elements().is_empty())
                .take(MEASURED_LETTERS)
                .map(|letter| {
                    use ui::vello::kurbo::Shape as _;
                    let b = (mark.affine() * letter.path).bounding_box();
                    json!({"t": letter.text, "box": [round(b.x0), round(b.y0), round(b.width()), round(b.height())]})
                })
                .collect();
            fields.insert("letters".into(), letters.into());
        }
        out.push(entry);
    }
    Ok(Value::Array(out))
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
/// name one, and its shape, style and transform when `full`.
fn mark_entry(art: &Artwork, mark: &Mark, named_layer: bool, full: bool) -> Value {
    let bounds = art.visible_bounds(mark);
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
        if let Some(gradient) = &mark.style.gradient {
            // Its colours, first stop to last; `full` gives the whole.
            let colours: Vec<&str> = gradient
                .stops
                .iter()
                .map(|(_, colour)| colour.as_str())
                .collect();
            fields.insert("gradient".into(), colours.into());
        } else if let Some(fill) = &mark.style.fill {
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
        if full {
            fields.insert("shape".into(), json!(mark.shape));
            fields.insert("style".into(), json!(mark.style));
            if let Some(transform) = mark.transform {
                fields.insert("transform".into(), json!(transform));
            }
        }
    }
    entry
}

/// The marks or groups `ids` (and the marks they are cut to) as an artwork
/// of their own, cropped to what they show, shadows included.
///
/// # Errors
/// Returns an id that names no mark or group.
pub fn part(art: &Artwork, ids: &[String]) -> Result<Artwork, String> {
    let mut chosen = std::collections::BTreeSet::new();
    for id in ids {
        let before = chosen.len();
        if art.mark(id).is_some() {
            chosen.insert(id.as_str());
        }
        chosen.extend(art.members(id).map(|mark| mark.id.as_str()));
        if chosen.len() == before && !chosen.contains(id.as_str()) {
            return Err(format!(
                "there is no mark or group {id:?}; inspect lists them"
            ));
        }
    }
    let clips: Vec<&str> = art
        .marks
        .iter()
        .filter(|mark| chosen.contains(mark.id.as_str()) && !mark.clip.is_empty())
        .map(|mark| mark.clip.as_str())
        .collect();
    chosen.extend(clips);
    let marks: Vec<&Mark> = art
        .marks
        .iter()
        .filter(|mark| chosen.contains(mark.id.as_str()))
        .collect();
    let shown = marks
        .iter()
        .filter(|mark| mark.style.opacity() > 0.0)
        .map(|mark| {
            let bounds = art.visible_bounds(mark);
            mark.style.shadow.as_ref().map_or(bounds, |shadow| {
                bounds.union(
                    bounds
                        .with_origin((bounds.x0 + shadow.offset[0], bounds.y0 + shadow.offset[1]))
                        .inflate(shadow.blur, shadow.blur),
                )
            })
        })
        .reduce(|a, b| a.union(b))
        .ok_or("those marks show nothing to export")?
        .expand();
    Ok(Artwork {
        width: shown.width().max(1.0),
        height: shown.height().max(1.0),
        marks: marks
            .into_iter()
            .map(|mark| mark.moved(-shown.x0, -shown.y0))
            .collect(),
        reference: None,
        ..art.clone()
    })
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
        .map(|mark| {
            mark_entry(
                art,
                mark,
                inspect.layer.is_some(),
                inspect.full == Some(true),
            )
        })
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
                if layer.texture > 0.0 {
                    fields.insert("texture".into(), layer.texture.into());
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
        "groups": art.groups(),
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
        "textures": tilo_wet_media::Texture::ALL.map(tilo_wet_media::Texture::name),
        "fonts": crate::fonts::listing(),
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

/// Ids added, removed and changed between two versions; each changed one
/// with what changed about it.
#[derive(Default)]
struct Changes {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<(String, Vec<String>)>,
}

impl Changes {
    fn of<T>(
        before: &[T],
        after: &[T],
        id: fn(&T) -> &str,
        fields: fn(&T, &T) -> Vec<String>,
    ) -> Self {
        let mut changes = Self::default();
        for item in after {
            match before.iter().find(|old| id(old) == id(item)) {
                None => changes.added.push(id(item).to_owned()),
                Some(old) => {
                    let differ = fields(old, item);
                    if !differ.is_empty() {
                        changes.changed.push((id(item).to_owned(), differ));
                    }
                }
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
        self.added
            .iter()
            .chain(&self.removed)
            .chain(self.changed.iter().map(|(id, _)| id))
    }

    fn is_empty(&self) -> bool {
        self.ids().next().is_none()
    }

    fn json(&self) -> Value {
        let changed: serde_json::Map<String, Value> = self
            .changed
            .iter()
            .map(|(id, fields)| (id.clone(), json!(fields)))
            .collect();
        json!({"added": self.added, "removed": self.removed, "changed": changed})
    }
}

/// What differs between two versions of a mark: `stacking` for its place
/// among the marks, a style's own keys (`fill`, `opacity`) for its style.
fn mark_fields(old: &Mark, new: &Mark) -> Vec<String> {
    let mut fields = Vec::new();
    for (differs, name) in [
        (old.layer != new.layer, "layer"),
        (old.order != new.order, "stacking"),
        (old.name != new.name, "name"),
        (old.shape != new.shape, "shape"),
        (old.transform != new.transform, "transform"),
        (old.group != new.group, "group"),
        (old.clip != new.clip, "clip"),
    ] {
        if differs {
            fields.push(name.to_owned());
        }
    }
    if old.style != new.style {
        let (Value::Object(was), Value::Object(now)) = (json!(old.style), json!(new.style)) else {
            fields.push("style".into());
            return fields;
        };
        let keys: std::collections::BTreeSet<&String> = was.keys().chain(now.keys()).collect();
        fields.extend(
            keys.into_iter()
                .filter(|key| was.get(*key) != now.get(*key))
                .cloned(),
        );
    }
    fields
}

/// What differs between two versions of a layer.
fn layer_fields(old: &Layer, new: &Layer) -> Vec<String> {
    [
        (old.name != new.name, "name"),
        (old.order != new.order, "stacking"),
        (old.medium != new.medium, "medium"),
        (old.opacity.to_bits() != new.opacity.to_bits(), "opacity"),
        (old.visible != new.visible, "visible"),
        (old.texture.to_bits() != new.texture.to_bits(), "texture"),
    ]
    .into_iter()
    .filter(|(differs, _)| *differs)
    .map(|(_, name)| name.to_owned())
    .collect()
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
    let marks = Changes::of(
        &before.marks,
        &after.marks,
        |mark: &Mark| mark.id.as_str(),
        mark_fields,
    );
    let layers = Changes::of(
        &before.layers,
        &after.layers,
        |layer: &Layer| layer.id.as_str(),
        layer_fields,
    );
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
