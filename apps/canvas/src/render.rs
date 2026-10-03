//! Drawing the artwork into a vector scene.
//!
//! Plain layers are vector art drawn directly. Painted layers are simulated
//! by their medium into a raster the size of the simulation grid and drawn
//! as an image with the medium's blend. The simulations are kept between
//! frames, so adding a stroke simulates only that stroke.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use std::collections::HashMap;
use std::sync::Arc;

use tilo_wet_media::{
    Blend, Canvas, ColourField, Engines, FieldKind, Fill as WetFill, Heightfield, LayerPainter,
    Mark as WetMark, Painted, Pigment, Settings, Stroke as WetStroke, StrokePoint as WetPoint,
    Texture,
};
use ui::TextRenderer;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, BezPath, Cap, Join, PathEl, Point, Rect, Stroke};
use ui::vello::peniko::{
    BlendMode, Blob, Color, ColorStop, Compose, Extend, Fill, Gradient as PenikoGradient,
    ImageAlphaType, ImageBrush, ImageData, ImageFormat, Mix,
};

use crate::media;
use crate::model::{Artwork, Gradient, Layer, Mark, Shape};

/// The longest side of a painted layer's simulation grid, in cells. Painted
/// layers simulate at most this finely; an export scales the result.
const MAX_CELLS: f64 = 1400.0; // a simulation budget in cells, not on-screen spacing

/// Cells per artwork unit for `art`'s painted layers.
pub fn cell_scale(art: &Artwork) -> f64 {
    (MAX_CELLS / art.width.max(art.height).max(1.0)).min(1.0)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "grid sides are clamped to 1..=MAX_CELLS before the cast"
)]
fn cells(units: f64, scale: f64) -> usize {
    (units * scale).round().clamp(1.0, MAX_CELLS) as usize
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "simulation coordinates are bounded artwork units"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// A colour a mark names, as drawn.
pub fn colour(text: &str, palette: &[Pigment]) -> Option<Color> {
    let paint = tilo_wet_media::parse_paint(text, palette)?;
    let [r, g, b] = paint.colour.to_srgb8();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "alpha is clamped to 0..=1 before scaling to a byte"
    )]
    let alpha = (paint.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color::from_rgba8(r, g, b, alpha))
}

fn with_opacity(colour: Color, opacity: f64) -> Color {
    colour.multiply_alpha(narrow(opacity.clamp(0.0, 1.0)))
}

struct PaintedLayer {
    painter: LayerPainter,
    key: blake3::Hash,
    image: Option<ImageData>,
}

/// Keeps what is expensive between frames: the lit ground, each painted
/// layer's simulation and the decoded reference picture.
#[derive(Default)]
pub struct Renderer {
    ground: Option<(String, Arc<Heightfield>, ImageData)>,
    painted: HashMap<String, PaintedLayer>,
    reference: Option<(String, Option<ImageData>)>,
    wet: Option<WetPreview>,
}

/// The last picture of a stroke still being painted, and what it cost, so
/// a slow medium is previewed only as often as it can keep up.
struct WetPreview {
    layer: String,
    image: ImageData,
    at: ui::time::Instant,
    cost: ui::time::Duration,
}

/// The least time between two previews of a stroke being painted.
const WET_INTERVAL: ui::time::Duration = ui::time::Duration::from_millis(16);

/// What a scene shows.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Only these layers, when given.
    pub layers: Option<Vec<String>>,
    /// A labelled coordinate grid over the artwork.
    pub grid: bool,
    /// The part of the artwork being shown, which the grid labels along its
    /// edges; the whole artwork when absent.
    pub view: Option<Rect>,
    /// The reference picture over the artwork, when there is one.
    pub reference: bool,
    /// Leave the ground out: the layers over transparency.
    pub bare: bool,
    /// A stroke still being painted: shown on its layer as its medium will
    /// paint it, without being kept.
    pub wet: Option<Mark>,
}

/// The largest reference picture, in bytes.
pub const MAX_REFERENCE_BYTES: usize = 40 << 20;
/// The ground where an artwork's colour cannot be read: white primed
/// canvas, the artwork's own colour, the same in every theme.
pub const BLANK_GROUND: Color = Color::WHITE; // document colour
/// A shadow's colour where its mark names none; the artwork's, not the
/// theme's.
pub const SHADOW_INK: Color = Color::from_rgba8(0, 0, 0, 96); // document colour

/// A PNG or JPEG decoded to straight RGBA, at most 8192 pixels a side.
///
/// # Errors
/// Returns why the bytes are not a picture.
pub fn decode_picture(bytes: &[u8]) -> Result<ImageData, String> {
    let format = image::guess_format(bytes).map_err(|error| error.to_string())?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err(format!("{format:?} is not PNG or JPEG"));
    }
    let decoded =
        image::load_from_memory_with_format(bytes, format).map_err(|error| error.to_string())?;
    if decoded.width().max(decoded.height()) > 8192 {
        return Err("the picture is over 8192 pixels a side".into());
    }
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(ImageData {
        data: Blob::new(Arc::new(rgba.into_raw())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    })
}

impl Renderer {
    /// Draw `art` into `scene`, artwork units mapped by `transform`.
    pub fn draw(
        &mut self,
        art: &Artwork,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        options: &Options,
    ) {
        let board = art.bounds();
        if !options.bare {
            self.draw_ground(art, scene, transform);
        }
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &board);
        let live: Vec<String> = art.layers.iter().map(|layer| layer.id.clone()).collect();
        self.painted.retain(|id, _| live.contains(id));
        for layer in &art.layers {
            let shown = options
                .layers
                .as_ref()
                .is_none_or(|only| only.contains(&layer.id));
            if !layer.visible || !shown {
                continue;
            }
            if let Some(medium) =
                media::resolve(art, &layer.medium).filter(|_| !layer.medium.is_empty())
            {
                let wet = options.wet.as_ref().filter(|wet| wet.layer == layer.id);
                self.draw_painted(art, (layer, wet), &medium, scene, transform, text);
            } else {
                let palette = media::palette_for(art, &layer.id);
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    narrow(layer.opacity),
                    transform,
                    &board,
                );
                for mark in art.marks_of(&layer.id) {
                    draw_mark(art, mark, &palette, scene, (transform, text));
                }
                scene.pop_layer();
            }
        }
        scene.pop_layer();
        if options.reference
            && let Some(reference) = &art.reference
        {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                narrow(reference.opacity),
                transform,
                &board,
            );
            self.draw_reference(art, scene, transform);
            scene.pop_layer();
        }
        if options.grid {
            draw_grid(art, options.view.unwrap_or(board), scene, transform, text);
        }
    }

    /// The colour of the paint at `at`: the topmost shown layer's there,
    /// else the ground's. A painted layer answers from its simulation as last
    /// drawn.
    pub fn sample(&mut self, art: &Artwork, at: Point) -> Color {
        for layer in art.layers.iter().rev().filter(|layer| layer.visible) {
            if let Some(entry) = self.painted.get(&layer.id)
                && let Some(image) = &entry.image
                && let Some(colour) = pixel(image, Affine::scale(cell_scale(art)) * at)
                && colour.components[3] > 0.06
            {
                return colour.with_alpha(1.0);
            }
            let palette = media::palette_for(art, &layer.id);
            let hit = art
                .marks_of(&layer.id)
                .filter(|mark| mark.hit(at) && art.clip_of(mark).is_none_or(|clip| clip.hit(at)));
            if let Some(mark) = hit.last() {
                let style = &mark.style;
                let named = if mark.shape.closed() || matches!(mark.shape, Shape::Text { .. }) {
                    style.fill.as_ref().or(style.stroke.as_ref())
                } else {
                    style.stroke.as_ref().or(style.fill.as_ref())
                };
                let gradient = style
                    .gradient
                    .as_ref()
                    .and_then(|gradient| gradient.stops.first())
                    .map(|(_, colour)| colour);
                if let Some(colour) = named.or(gradient).and_then(|named| colour(named, &palette)) {
                    return colour.with_alpha(1.0);
                }
            }
        }
        if Texture::parse(&art.ground.texture).is_some_and(|texture| texture != Texture::Smooth)
            && let Some(colour) = pixel(
                &self.ground(art).2.clone(),
                Affine::scale(cell_scale(art)) * at,
            )
        {
            return colour;
        }
        colour(&art.ground.colour, &[]).unwrap_or(BLANK_GROUND)
    }

    /// The reference picture, decoded once per picture.
    pub fn reference_image(&mut self, art: &Artwork) -> Option<ImageData> {
        let address = &art.reference.as_ref()?.image;
        if self
            .reference
            .as_ref()
            .is_none_or(|(have, _)| have != address)
        {
            let image = ui::tilo_actions::content_hash(address)
                .and_then(|hash| tilo_content::ContentStore::for_app().read(hash))
                .and_then(|bytes| decode_picture(&bytes).ok());
            self.reference = Some((address.clone(), image));
        }
        self.reference.as_ref().and_then(|(_, image)| image.clone())
    }

    /// Where the reference lies: its frame, or fitted to the artwork with
    /// its aspect kept.
    pub fn reference_frame(&mut self, art: &Artwork) -> Option<Rect> {
        let image = self.reference_image(art)?;
        if let Some([x, y, width, height]) = art.reference.as_ref()?.frame {
            return Some(Rect::new(x, y, x + width, y + height));
        }
        let (width, height) = (f64::from(image.width), f64::from(image.height));
        let fit = (art.width / width).min(art.height / height);
        let (w, h) = (width * fit, height * fit);
        let (x, y) = ((art.width - w) / 2.0, (art.height - h) / 2.0);
        Some(Rect::new(x, y, x + w, y + h))
    }

    /// Draw the reference picture alone, fully opaque, into its frame.
    pub fn draw_reference(&mut self, art: &Artwork, scene: &mut Scene, transform: Affine) {
        let (Some(image), Some(frame)) = (self.reference_image(art), self.reference_frame(art))
        else {
            return;
        };
        let to_frame = Affine::translate(frame.origin().to_vec2())
            * Affine::scale_non_uniform(
                frame.width() / f64::from(image.width),
                frame.height() / f64::from(image.height),
            );
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(image),
            Some(to_frame),
            &frame,
        );
    }

    fn draw_ground(&mut self, art: &Artwork, scene: &mut Scene, transform: Affine) {
        let board = art.bounds();
        let base = colour(&art.ground.colour, &[]).unwrap_or(BLANK_GROUND);
        let texture = Texture::parse(&art.ground.texture).unwrap_or_default();
        if texture == Texture::Smooth {
            scene.fill(Fill::NonZero, transform, base, None, &board);
            return;
        }
        let image = self.ground(art).2.clone();
        let scale = cell_scale(art);
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(image),
            Some(Affine::scale(1.0 / scale)),
            &board,
        );
    }

    /// The ground's tooth and its lit image, built once per ground.
    fn ground(&mut self, art: &Artwork) -> &(String, Arc<Heightfield>, ImageData) {
        let scale = cell_scale(art);
        let (width, height) = (cells(art.width, scale), cells(art.height, scale));
        let key = format!(
            "{}:{}:{width}x{height}",
            art.ground.texture, art.ground.colour
        );
        if self.ground.as_ref().is_none_or(|(have, ..)| *have != key) {
            let texture = Texture::parse(&art.ground.texture).unwrap_or_default();
            let field = Arc::new(Heightfield::new(width, height, texture, narrow(scale), 1));
            let lit = tilo_wet_media::parse_paint(&art.ground.colour, &[])
                .map_or(tilo_wet_media::Rgb::WHITE, |paint| paint.colour);
            let image = image(field.shade(lit), width, height);
            self.ground = Some((key, field, image));
        }
        #[allow(clippy::expect_used, reason = "set just above when absent")]
        self.ground.as_ref().expect("ground built")
    }

    fn draw_painted(
        &mut self,
        art: &Artwork,
        (layer, wet): (&Layer, Option<&Mark>),
        medium: &media::MediumSpec,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
    ) {
        let engines = Engines::builtin();
        let Some(engine) = engines.get(&medium.engine) else {
            return;
        };
        let scale = cell_scale(art);
        let ground = Arc::clone(&self.ground(art).1);
        let canvas = Canvas {
            width: ground.width,
            height: ground.height,
            scale: narrow(scale),
            ground,
        };
        let preset = medium.to_preset();
        let settings = Settings::new(engine.parameters(), &preset.params);
        let palette = preset.palette;
        let marks: Vec<Painted> = art
            .marks_of(&layer.id)
            .flat_map(|mark| wet_marks(mark, &palette, scale))
            .collect();
        let mut key = blake3::Hasher::new();
        for mark in &marks {
            key.update(&mark.fingerprint);
        }
        key.update(medium.engine.as_bytes());
        key.update(format!("{:?}", settings.resolved()).as_bytes());
        let key = key.finalize();
        let entry = self
            .painted
            .entry(layer.id.clone())
            .or_insert_with(|| PaintedLayer {
                painter: LayerPainter::new(Arc::clone(&engine), canvas.clone(), settings.clone()),
                key,
                image: None,
            });
        if !entry.painter.matches(engine.id(), &canvas, &settings) {
            entry.painter = LayerPainter::new(Arc::clone(&engine), canvas.clone(), settings);
            entry.image = None;
        }
        if entry.image.is_none() || entry.key != key {
            let pixels = entry.painter.paint(&marks).to_vec();
            entry.image = Some(image(pixels, canvas.width, canvas.height));
            entry.key = key;
        }
        let shown = if let Some(wet) = wet {
            let now = ui::time::now();
            let due = self.wet.as_ref().is_none_or(|preview| {
                preview.layer != layer.id
                    || now.duration_since(preview.at) >= (preview.cost * 2).max(WET_INTERVAL)
            });
            if due {
                let strokes = wet_marks(wet, &palette, scale);
                let pixels = entry.painter.preview(&marks, &strokes);
                self.wet = Some(WetPreview {
                    layer: layer.id.clone(),
                    image: image(pixels, canvas.width, canvas.height),
                    at: now,
                    cost: ui::time::now().duration_since(now),
                });
            }
            self.wet.as_ref().map(|preview| preview.image.clone())
        } else {
            if self
                .wet
                .as_ref()
                .is_some_and(|preview| preview.layer == layer.id)
            {
                self.wet = None;
            }
            entry.image.clone()
        };
        let blend = match engine.blend() {
            Blend::Normal => BlendMode::default(),
            Blend::Multiply => BlendMode::new(Mix::Multiply, Compose::SrcOver),
        };
        let board = art.bounds();
        scene.push_layer(
            Fill::NonZero,
            blend,
            narrow(layer.opacity),
            transform,
            &board,
        );
        if let Some(image) = shown {
            scene.fill(
                Fill::NonZero,
                transform,
                &ImageBrush::new(image),
                Some(Affine::scale(1.0 / scale)),
                &board,
            );
        }
        // Lettering is not paint: it stays crisp on a painted layer.
        for mark in art.marks_of(&layer.id) {
            if matches!(mark.shape, Shape::Text { .. }) {
                draw_vector(mark, &palette, scene, transform, text);
            }
        }
        scene.pop_layer();
    }
}

/// The straight-alpha pixel of `image` at `at` (pixel coordinates).
fn pixel(image: &ImageData, at: Point) -> Option<Color> {
    if at.x < 0.0 || at.y < 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "non-negative and checked against the image's size below"
    )]
    let (column, row) = (at.x.floor() as u32, at.y.floor() as u32);
    if column >= image.width || row >= image.height {
        return None;
    }
    let start =
        usize::try_from((u64::from(row) * u64::from(image.width) + u64::from(column)) * 4).ok()?;
    let rgba = image.data.data().get(start..start.checked_add(4)?)?;
    let [red, green, blue, alpha] = <[u8; 4]>::try_from(rgba).ok()?;
    Some(Color::from_rgba8(red, green, blue, alpha))
}

fn image(pixels: Vec<u8>, width: usize, height: usize) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(pixels)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: u32::try_from(width).unwrap_or(1),
        height: u32::try_from(height).unwrap_or(1),
    }
}

/// The outline of `path` flattened into polygons, in cell coordinates.
fn contours(path: &BezPath, to_cells: Affine) -> Vec<Vec<(f32, f32)>> {
    let mut contours: Vec<Vec<(f32, f32)>> = Vec::new();
    ui::vello::kurbo::flatten(to_cells * path.clone(), 0.25, |element| match element {
        PathEl::MoveTo(at) => contours.push(vec![(narrow(at.x), narrow(at.y))]),
        PathEl::LineTo(at) => {
            if let Some(contour) = contours.last_mut() {
                contour.push((narrow(at.x), narrow(at.y)));
            }
        }
        _ => {}
    });
    contours.retain(|contour| contour.len() >= 2);
    contours
}

/// What a mark is as paint: a laid-in region, a stroke, or both.
fn wet_marks(mark: &Mark, palette: &[Pigment], scale: f64) -> Vec<Painted> {
    let style = &mark.style;
    let to_cells = Affine::scale(scale) * mark.affine();
    let params = style
        .params
        .iter()
        .map(|(id, value)| (id.clone(), narrow(*value)))
        .collect();
    let opacity = narrow(style.opacity());
    let paint = |text: &Option<String>| {
        text.as_deref()
            .and_then(|text| tilo_wet_media::parse_paint(text, palette))
    };
    let mut painted = Vec::new();
    match &mark.shape {
        Shape::Stroke { points, size } => {
            if let Some(colour) = paint(&style.stroke).or_else(|| paint(&style.fill)) {
                let points = points
                    .iter()
                    .map(|point| {
                        let at = to_cells
                            * Point::new(
                                point.first().copied().unwrap_or(0.0),
                                point.get(1).copied().unwrap_or(0.0),
                            );
                        WetPoint::new(
                            narrow(at.x),
                            narrow(at.y),
                            narrow(point.get(2).copied().unwrap_or(1.0)),
                        )
                    })
                    .collect();
                painted.push(Painted::new(
                    &mark.id,
                    WetMark::Stroke(WetStroke {
                        points,
                        size: narrow(size * scale),
                        paint: colour,
                        opacity,
                        wet: style.wet,
                        params,
                    }),
                ));
            }
        }
        Shape::Text { .. } => {}
        shape => {
            let path = shape.path();
            if shape.closed()
                && let Some(colour) = paint(&style.fill).or_else(|| {
                    style
                        .gradient
                        .as_ref()
                        .and_then(|gradient| gradient.stops.first())
                        .and_then(|(_, colour)| tilo_wet_media::parse_paint(colour, palette))
                        // The stops' alpha fades the paint across the
                        // mark; it is not the whole mark's opacity.
                        .map(|paint| tilo_wet_media::Paint {
                            alpha: 1.0,
                            ..paint
                        })
                })
            {
                let bounds = mark.bounds();
                let brush = style.brush.unwrap_or_else(|| {
                    (bounds.width().min(bounds.height()) / 5.0).clamp(6.0, 48.0)
                });
                painted.push(Painted::new(
                    &mark.id,
                    WetMark::Fill(WetFill {
                        contours: contours(&path, to_cells),
                        paint: colour,
                        opacity,
                        wet: style.wet,
                        brush: narrow(brush * scale),
                        angle: narrow(style.angle.unwrap_or(0.0)),
                        params: params.clone(),
                        field: style
                            .gradient
                            .as_ref()
                            .and_then(|gradient| colour_field(gradient, palette, to_cells)),
                    }),
                ));
            }
            let outline = (style.stroke_width > 0.0 || !shape.closed())
                .then(|| paint(&style.stroke))
                .flatten();
            if let Some(colour) = outline {
                painted.extend(outline_strokes(
                    mark, &path, to_cells, colour, scale, &params,
                ));
            }
        }
    }
    painted
}

/// A mark's gradient as the colour its paint is mixed to across it, in
/// cell coordinates.
fn colour_field(gradient: &Gradient, palette: &[Pigment], to_cells: Affine) -> Option<ColourField> {
    let stops = gradient
        .stops
        .iter()
        .filter_map(|(at, text)| {
            let paint = tilo_wet_media::parse_paint(text, palette)?;
            Some((narrow(*at), paint.colour, paint.alpha))
        })
        .collect();
    let kind = if gradient.kind == "radial" {
        FieldKind::Radial
    } else {
        FieldKind::Linear
    };
    let from = to_cells * Point::new(gradient.from[0], gradient.from[1]);
    let to = to_cells * Point::new(gradient.to[0], gradient.to[1]);
    ColourField::new(
        kind,
        (narrow(from.x), narrow(from.y)),
        (narrow(to.x), narrow(to.y)),
        stops,
    )
}

/// A shape's outline as strokes of its stroke colour.
fn outline_strokes(
    mark: &Mark,
    path: &BezPath,
    to_cells: Affine,
    colour: tilo_wet_media::Paint,
    scale: f64,
    params: &std::collections::BTreeMap<String, f32>,
) -> Vec<Painted> {
    let closed = mark.shape.closed();
    contours(path, to_cells)
        .into_iter()
        .map(|contour| {
            let mut points: Vec<WetPoint> = contour
                .iter()
                .map(|(x, y)| WetPoint::new(*x, *y, 1.0))
                .collect();
            if closed && let Some(first) = points.first().copied() {
                points.push(first);
            }
            Painted::new(
                &format!("{}/outline", mark.id),
                WetMark::Stroke(WetStroke {
                    points,
                    size: narrow(mark.style.stroke_width.max(1.0) * scale),
                    paint: colour,
                    opacity: narrow(mark.style.opacity()),
                    wet: true,
                    params: params.clone(),
                }),
            )
        })
        .collect()
}

fn gradient_brush(
    gradient: &Gradient,
    palette: &[Pigment],
    opacity: f64,
) -> Option<PenikoGradient> {
    let stops: Vec<ColorStop> = gradient
        .stops
        .iter()
        .filter_map(|(at, text)| {
            Some(ColorStop {
                offset: narrow(at.clamp(0.0, 1.0)),
                color: with_opacity(colour(text, palette)?, opacity).into(),
            })
        })
        .collect();
    if stops.len() < 2 {
        return None;
    }
    let from = Point::new(gradient.from[0], gradient.from[1]);
    let to = Point::new(gradient.to[0], gradient.to[1]);
    let brush = if gradient.kind == "radial" {
        PenikoGradient::new_radial(from, narrow(from.distance(to)))
    } else {
        PenikoGradient::new_linear(from, to)
    };
    Some(brush.with_extend(Extend::Pad).with_stops(&stops[..]))
}

/// The blend a mark names.
pub fn mix(blend: &str) -> Option<Mix> {
    Some(match blend {
        "multiply" => Mix::Multiply,
        "screen" => Mix::Screen,
        "overlay" => Mix::Overlay,
        "darken" => Mix::Darken,
        "lighten" => Mix::Lighten,
        "color-dodge" => Mix::ColorDodge,
        "color-burn" => Mix::ColorBurn,
        "hard-light" => Mix::HardLight,
        "soft-light" => Mix::SoftLight,
        "difference" => Mix::Difference,
        "exclusion" => Mix::Exclusion,
        "hue" => Mix::Hue,
        "saturation" => Mix::Saturation,
        "color" => Mix::Color,
        "luminosity" => Mix::Luminosity,
        _ => return None,
    })
}

/// The region a shape covers, untransformed: a brush stroke's inked
/// outline, a text's box, any other shape's outline.
pub fn silhouette(shape: &Shape) -> BezPath {
    match shape {
        Shape::Stroke { points, size } => ink_path(points, *size),
        shape => shape.path(),
    }
}

fn ink_path(points: &[Vec<f64>], size: f64) -> BezPath {
    let points: Vec<WetPoint> = points
        .iter()
        .map(|point| {
            WetPoint::new(
                narrow(point.first().copied().unwrap_or(0.0)),
                narrow(point.get(1).copied().unwrap_or(0.0)),
                narrow(point.get(2).copied().unwrap_or(1.0)),
            )
        })
        .collect();
    let samples = tilo_wet_media::brush::smooth(&points, 0.75);
    let outline = tilo_wet_media::brush::ink_outline(&samples, narrow(size));
    let mut path = BezPath::new();
    for (index, (x, y)) in outline.iter().enumerate() {
        let point = Point::new(f64::from(*x), f64::from(*y));
        if index == 0 {
            path.move_to(point);
        } else {
            path.line_to(point);
        }
    }
    path.close_path();
    path
}

/// Draw one mark of a plain layer: cut to its clip, mixed by its blend,
/// over its shadow.
fn draw_mark(
    art: &Artwork,
    mark: &Mark,
    palette: &[Pigment],
    scene: &mut Scene,
    (transform, text): (Affine, &mut TextRenderer),
) {
    let mut layers = 0;
    if let Some(clip) = art.mark(&mark.clip).filter(|_| !mark.clip.is_empty()) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform * clip.affine(),
            &silhouette(&clip.shape),
        );
        layers += 1;
    }
    if let Some(mix) = mark.style.blend.as_deref().and_then(mix) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::new(mix, Compose::SrcOver),
            1.0,
            transform,
            &art.bounds(),
        );
        layers += 1;
    }
    if let Some(shadow) = &mark.style.shadow {
        draw_shadow(mark, shadow, palette, scene, (transform, text));
    }
    draw_vector(mark, palette, scene, transform, text);
    for _ in 0..layers {
        scene.pop_layer();
    }
}

/// How many rings a soft shadow is built from.
const SHADOW_RINGS: u16 = 8;

/// A shadow: the mark's silhouette offset and, when soft, grown outward in
/// rings that fade, so its edge falls off over `blur` units.
fn draw_shadow(
    mark: &Mark,
    shadow: &crate::model::Shadow,
    palette: &[Pigment],
    scene: &mut Scene,
    (transform, text): (Affine, &mut TextRenderer),
) {
    let ink = shadow
        .colour
        .as_deref()
        .and_then(|value| colour(value, palette))
        .unwrap_or(SHADOW_INK);
    let alpha = f64::from(ink.components[3]) * mark.style.opacity();
    let ink = ink.with_alpha(1.0);
    let at = transform * Affine::translate((shadow.offset[0], shadow.offset[1])) * mark.affine();
    let rings = if shadow.blur > 0.0 { SHADOW_RINGS } else { 1 };
    // Each ring lets through the share that compounds to `alpha` inside.
    let each = 1.0 - (1.0 - alpha.clamp(0.0, 0.999)).powf(1.0 / f64::from(rings));
    let reach = (mark.bounds() + ui::vello::kurbo::Vec2::new(shadow.offset[0], shadow.offset[1]))
        .inflate(shadow.blur + 2.0, shadow.blur + 2.0);
    if let Shape::Text { .. } = mark.shape {
        for ring in 0..rings {
            let turn = f64::from(ring) / f64::from(rings) * std::f64::consts::TAU;
            let jitter = Affine::translate((
                turn.cos() * shadow.blur * 0.5,
                turn.sin() * shadow.blur * 0.5,
            ));
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                narrow(each),
                transform,
                &reach,
            );
            draw_text_shape(&mark.shape, ink, scene, jitter * at, text);
            scene.pop_layer();
        }
        return;
    }
    let filled = mark.shape.closed() || matches!(mark.shape, Shape::Stroke { .. });
    let path = silhouette(&mark.shape);
    let width = if filled {
        0.0
    } else {
        mark.style.stroke_width.max(1.0)
    };
    for ring in 0..rings {
        let grow = if rings > 1 {
            shadow.blur * (f64::from(ring) + 0.5) / f64::from(rings)
        } else {
            0.0
        };
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            narrow(each),
            transform,
            &reach,
        );
        if filled {
            scene.fill(Fill::NonZero, at, ink, None, &path);
        }
        if width + grow > 0.0 {
            let pen = Stroke::new(width + grow * 2.0)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            scene.stroke(&pen, at, ink, None, &path);
        }
        scene.pop_layer();
    }
}

/// The toolkit's text style for a text shape.
pub fn text_style(shape: &Shape) -> Option<ui::TextStyle> {
    let Shape::Text {
        size,
        font,
        weight,
        italic,
        align,
        line_height,
        ..
    } = shape
    else {
        return None;
    };
    let family = match font.as_deref() {
        None | Some("sans") => ui::FontChoice::SansSerif,
        Some("serif") => ui::FontChoice::Serif,
        Some("mono") => ui::FontChoice::Monospace,
        Some(name) => ui::FontChoice::Named(name.to_owned()),
    };
    let weight = match weight.unwrap_or(400) {
        0..=449 => ui::TextWeight::Regular,
        450..=549 => ui::TextWeight::Medium,
        550..=649 => ui::TextWeight::Semibold,
        _ => ui::TextWeight::Bold,
    };
    let align = match align.as_deref() {
        Some("center") => ui::ParagraphAlign::Center,
        Some("end") => ui::ParagraphAlign::End,
        _ => ui::ParagraphAlign::Start,
    };
    Some(
        ui::TextStyle::new(narrow(*size))
            .with_family(family)
            .with_weight(weight)
            .with_italic(*italic)
            .with_line_height(narrow(line_height.unwrap_or(1.2)))
            .with_align(align),
    )
}

/// Shape and paint a text mark with the toolkit's text renderer.
fn draw_text_shape(
    shape: &Shape,
    ink: Color,
    scene: &mut Scene,
    at: Affine,
    text: &mut TextRenderer,
) {
    let (
        Some(style),
        Shape::Text {
            x,
            y,
            text: words,
            align,
            width,
            ..
        },
    ) = (text_style(shape), shape)
    else {
        return;
    };
    let natural = f64::from(text.measure_style(words, width.map(narrow), &style).0);
    let wide = width.unwrap_or(natural + 0.5);
    let left = match (width, align.as_deref()) {
        (None, Some("center")) => x - wide / 2.0,
        (None, Some("end")) => x - wide,
        _ => *x,
    };
    let max = (width.is_some() || align.as_deref().is_some_and(|align| align != "start"))
        .then(|| narrow(wide));
    let top = y - f64::from(text.first_baseline_style(words, max, &style));
    text.draw_style(
        scene,
        words,
        ink,
        at * Affine::translate((left, top)),
        max,
        &style,
    );
}

/// The pen a mark's outline is drawn with.
fn pen(style: &crate::model::Style, width: f64) -> Stroke {
    let cap = match style.cap.as_deref() {
        Some("butt") => Cap::Butt,
        Some("square") => Cap::Square,
        _ => Cap::Round,
    };
    let join = match style.join.as_deref() {
        Some("miter") => Join::Miter,
        Some("bevel") => Join::Bevel,
        _ => Join::Round,
    };
    let pen = Stroke::new(width).with_caps(cap).with_join(join);
    if style.dash.is_empty() {
        pen
    } else {
        pen.with_dashes(0.0, style.dash.iter().copied())
    }
}

/// Draw one mark as vector art.
pub fn draw_vector(
    mark: &Mark,
    palette: &[Pigment],
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
) {
    let style = &mark.style;
    let at = transform * mark.affine();
    let opacity = style.opacity();
    let named = |colour_text: &Option<String>| {
        colour_text
            .as_deref()
            .and_then(|value| colour(value, palette))
            .map(|value| with_opacity(value, opacity))
    };
    match &mark.shape {
        Shape::Stroke { points, size } => {
            let Some(ink) = named(&style.stroke).or_else(|| named(&style.fill)) else {
                return;
            };
            scene.fill(Fill::NonZero, at, ink, None, &ink_path(points, *size));
        }
        Shape::Text { .. } => {
            let Some(ink) = named(&style.fill).or_else(|| named(&style.stroke)) else {
                return;
            };
            draw_text_shape(&mark.shape, ink, scene, at, text);
        }
        shape => {
            let path = shape.path();
            if shape.closed() {
                if let Some(gradient) = style
                    .gradient
                    .as_ref()
                    .and_then(|gradient| gradient_brush(gradient, palette, opacity))
                {
                    scene.fill(Fill::NonZero, at, &gradient, None, &path);
                } else if let Some(fill) = named(&style.fill) {
                    scene.fill(Fill::NonZero, at, fill, None, &path);
                }
            }
            let width = if shape.closed() {
                style.stroke_width
            } else {
                style.stroke_width.max(1.0)
            };
            if width > 0.0
                && let Some(line) = named(&style.stroke)
            {
                scene.stroke(&pen(style, width), at, line, None, &path);
            }
        }
    }
}

/// A grid every `step` units with coordinates written along the top and
/// left of `view`, so a caller looking at the picture can read where things
/// are, also in a close look at a region.
fn draw_grid(
    art: &Artwork,
    view: Rect,
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
) {
    let view = view.intersect(art.bounds());
    let span = view.width().max(view.height());
    let step = [
        5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0,
    ]
    .into_iter()
    .find(|step| span / step <= 12.0)
    .unwrap_or(1000.0);
    let pixel = 1.0 / transform.as_coeffs()[0].abs().max(1e-6);
    // The grid is printed into a picture an agent reads, over any artwork:
    // it must look the same whatever theme the desktop wears.
    let line = Color::from_rgba8(30, 120, 220, 150); // picture ink, theme-independent
    let label = Color::from_rgba8(20, 80, 170, 255); // picture ink, theme-independent
    let halo = Color::from_rgba8(255, 255, 255, 210); // picture ink, theme-independent
    let pen = Stroke::new(pixel);
    let font = narrow((pixel * 11.0).max(0.1));
    let tag = |scene: &mut Scene, text: &mut TextRenderer, x: f64, y: f64, value: f64| {
        let words = format!("{value:.0}");
        let width = pixel * (6.5 * f64::from(u32::try_from(words.len()).unwrap_or(4)) + 4.0);
        scene.fill(
            Fill::NonZero,
            transform,
            halo,
            None,
            &Rect::new(x, y, x + width, y + pixel * 14.0),
        );
        text.draw_text(
            scene,
            &words,
            font,
            label,
            transform * Affine::translate((x + pixel * 2.0, y + pixel)),
            None,
        );
    };
    let lines = |from: f64, to: f64| {
        let first = (from / step).ceil();
        let count = ((to / step).floor() - first).max(-1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "at most a dozen grid lines, counted from clamped floats"
        )]
        let count = (count + 1.0).clamp(0.0, 64.0) as u32;
        (0..count).map(move |index| (first + f64::from(index)) * step)
    };
    for at in lines(view.x0, view.x1) {
        scene.stroke(
            &pen,
            transform,
            line,
            None,
            &ui::vello::kurbo::Line::new((at, view.y0), (at, view.y1)),
        );
        tag(scene, text, at + pixel * 2.0, view.y0 + pixel, at);
    }
    for at in lines(view.y0, view.y1) {
        scene.stroke(
            &pen,
            transform,
            line,
            None,
            &ui::vello::kurbo::Line::new((view.x0, at), (view.x1, at)),
        );
        if at > view.y0 + pixel * 16.0 {
            tag(scene, text, view.x0 + pixel * 2.0, at + pixel, at);
        }
    }
}
