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
use ui::num::f32_of_f64;

use tilo_wet_media::{
    Blend, Canvas, ColourField, Engines, FieldKind, Fill as WetFill, Heightfield, LayerPainter,
    Mark as WetMark, Painted, Pigment, Settings, Stroke as WetStroke, StrokePoint as WetPoint,
    Texture,
};
use ui::TextRenderer;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, BezPath, Cap, Join, PathEl, Point, Rect, Shape as _, Stroke};
use ui::vello::peniko::{
    BlendMode, Color, ColorStop, Compose, Extend, Fill, Gradient as PenikoGradient, ImageBrush,
    ImageData, Mix,
};

use crate::adjust::Grade;
use crate::detail::{self, Detail, LayerLevel, Mips, Request, Tiles, Work};
use crate::media;
use crate::model::{Adjust, Artwork, Gradient, Layer, Mark, Shape};
use crate::picture::{Pictures, Placement, Treatment};
use crate::settle::{Ask, Settle};

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
    colour.multiply_alpha(f32_of_f64(opacity.clamp(0.0, 1.0)))
}

struct PaintedLayer {
    /// The simulation, kept up with the marks away from the window.
    settle: Settle,
    /// The marks as paint, kept while they stay.
    sources: Sources,
    /// The painting graded by the layer's adjustments, and what they were.
    graded: Option<(String, ImageData)>,
    /// The picture's halvings, for a view farther than its scale.
    mips: Mips,
    /// The marks at the finer level a close view simulates.
    level: Option<LayerLevel>,
    /// The finer tiles shown, graded by the layer's adjustments, and what
    /// they and the adjustments were.
    graded_mosaic: Option<(u64, String, ImageData)>,
}

/// A painted layer's marks as its medium paints them, each kept while the
/// mark, what it is cut to and the palette and scale stay the same, so a
/// frame does not turn every mark into paint again.
#[derive(Default)]
struct Sources {
    /// The palette and scale the marks were turned into paint with.
    basis: String,
    marks: HashMap<String, (Mark, Option<BezPath>, Vec<Painted>)>,
    /// The marks' ids in order, and all of them as paint with its key.
    order: Vec<String>,
    laid: Option<(blake3::Hash, Arc<[Painted]>)>,
}

impl Sources {
    /// `layer`'s marks as paint, and their key.
    fn of(
        &mut self,
        art: &Artwork,
        layer: &str,
        palette: &[Pigment],
        scale: f64,
    ) -> (blake3::Hash, Arc<[Painted]>) {
        let basis = format!("{scale}|{palette:?}");
        if basis != self.basis {
            *self = Self {
                basis,
                ..Self::default()
            };
        }
        let mut changed = self.laid.is_none();
        let mut order = Vec::new();
        for mark in art.marks_of(layer) {
            let cut = wet_cut(art, mark);
            let kept = self
                .marks
                .get(&mark.id)
                .is_some_and(|(kept, kept_cut, _)| kept == mark && *kept_cut == cut);
            if !kept {
                let painted = wet_marks(mark, cut.as_ref(), palette, scale);
                self.marks
                    .insert(mark.id.clone(), (mark.clone(), cut, painted));
                changed = true;
            }
            order.push(mark.id.as_str());
        }
        if order != self.order {
            changed = true;
            self.order = order.into_iter().map(str::to_owned).collect();
            let order = &self.order;
            self.marks.retain(|id, _| order.contains(id));
        }
        if changed || self.laid.is_none() {
            let painted: Vec<Painted> = self
                .order
                .iter()
                .filter_map(|id| self.marks.get(id))
                .flat_map(|(_, _, painted)| painted.iter().cloned())
                .collect();
            let mut key = blake3::Hasher::new();
            for mark in &painted {
                key.update(&mark.fingerprint);
            }
            self.laid = Some((key.finalize(), painted.into()));
        }
        #[allow(clippy::expect_used, reason = "set just above when absent")]
        let (key, painted) = self.laid.as_ref().expect("marks laid");
        (*key, Arc::clone(painted))
    }
}

/// Keeps what is expensive between frames: the lit ground, each painted
/// layer's simulation and the decoded and graded pictures.
#[derive(Default)]
pub struct Renderer {
    ground: Option<(String, Arc<Heightfield>, ImageData)>,
    painted: HashMap<String, PaintedLayer>,
    pub pictures: Pictures,
    tooth: Option<(String, ImageData)>,
    brushes: BrushCache,
    paths: PathCache,
    /// The ground's halvings, for a view farther than its scale.
    ground_mips: Mips,
    /// Tiles of a close view, simulated finer than the whole layer.
    tiles: Tiles,
}

/// Parsed SVG geometry survives viewport redraws. Bound source bytes so an
/// agent replacing outlines repeatedly cannot keep unbounded old geometry.
#[derive(Default)]
struct PathCache {
    entries: HashMap<String, (String, Arc<BezPath>)>,
    bytes: usize,
}

impl PathCache {
    fn get(&mut self, mark: &Mark) -> Option<Arc<BezPath>> {
        const BUDGET: usize = 4 << 20;
        let Shape::Path { d } = &mark.shape else {
            return None;
        };
        if let Some((known, path)) = self.entries.get(&mark.id)
            && known == d
        {
            return Some(Arc::clone(path));
        }
        let path = Arc::new(mark.shape.path());
        if d.len() > BUDGET {
            return Some(path);
        }
        if let Some((old, _)) = self.entries.remove(&mark.id) {
            self.bytes = self.bytes.saturating_sub(old.len());
        }
        if self.bytes.saturating_add(d.len()) > BUDGET || self.entries.len() >= 512 {
            self.entries.clear();
            self.bytes = 0;
        }
        self.bytes = self.bytes.saturating_add(d.len());
        self.entries
            .insert(mark.id.clone(), (d.clone(), Arc::clone(&path)));
        Some(path)
    }
}

/// Dabbed strokes as last laid, by what they were laid from, so a frame
/// draws them without stamping them again.
#[derive(Default)]
struct BrushCache {
    entries: HashMap<[u8; 32], LaidDabs>,
    tick: u64,
    bytes: usize,
}

struct LaidDabs {
    image: ImageData,
    origin: Point,
    scale: f64,
    used: u64,
}

/// How many bytes of dabbed strokes are kept between frames.
const BRUSH_CACHE_BYTES: usize = 256 << 20;

impl BrushCache {
    /// Drop the least recently drawn strokes until the cache fits.
    fn trim(&mut self) {
        while self.bytes > BRUSH_CACHE_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, laid)| laid.used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(laid) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(laid.image.data.len());
            }
        }
    }
}

/// What drawing one mark of a plain layer needs besides the mark.
struct MarkCx<'a> {
    transform: Affine,
    text: &'a mut TextRenderer,
    tooth: Option<&'a Speckle>,
    brushes: &'a mut BrushCache,
    paths: &'a mut PathCache,
    /// The ground's height field and cells per unit, which graphite and
    /// charcoal catch on.
    ground: (&'a Heightfield, f64),
    /// Pictures decoded and graded, kept between frames.
    pictures: &'a mut Pictures,
    /// The adjustments of the layer the marks are on.
    layer_adjust: Option<&'a Adjust>,
}

/// What a scene shows.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the grid, the reference, the ground and the selection are shown independently"
)]
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
    /// Only these marks of plain layers, when given.
    pub marks: Option<Vec<String>>,
    /// The selection's outline over the artwork.
    pub selection: bool,
    /// How closely the artwork is shown: closer than a painted layer's
    /// simulation, the part shown is simulated finer; farther, its picture
    /// is drawn from halved copies.
    pub detail: Option<Detail>,
    /// A frame of a live view: a painted layer still being simulated shows
    /// its last picture (and a stroke just laid, its preview) rather than
    /// the window waiting for it. Anything else, a `look` or an export,
    /// waits for the exact picture.
    pub live: bool,
}

/// The ground where an artwork's colour cannot be read: white primed
/// canvas, the artwork's own colour, the same in every theme.
pub const BLANK_GROUND: Color = Color::WHITE; // document colour
/// What an eraser covers, as the mask that takes paint out: an eraser has
/// no colour, only its coverage counts.
const ERASED: Color = Color::TRANSPARENT.with_alpha(1.0);
/// A shadow's colour where its mark names none; the artwork's, not the
/// theme's.
pub const SHADOW_INK: Color = Color::from_rgba8(0, 0, 0, 96); // document colour

impl Renderer {
    /// A renderer that draws a placeholder for a picture still decoding,
    /// decoding it in the background, instead of waiting for it.
    pub fn impatient() -> Self {
        let mut renderer = Self::default();
        renderer.pictures.patient = false;
        renderer
    }

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
        self.tiles.begin();
        if !options.bare {
            self.draw_ground(art, scene, transform, options.detail.as_ref());
        }
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &board);
        let live: Vec<String> = art.layers.iter().map(|layer| layer.id.clone()).collect();
        self.painted.retain(|id, _| live.contains(id));
        let drawn: Vec<&Layer> = art.layers.iter().filter(|layer| !layer.is_mask()).collect();
        let shown = |layer: &Layer| {
            options
                .layers
                .as_ref()
                .is_none_or(|only| only.contains(&layer.id))
        };
        // A mask asked for by name shows as its greys.
        for mask in art
            .layers
            .iter()
            .filter(|layer| layer.is_mask() && options.layers.is_some() && shown(layer))
        {
            let grey = byte_of(mask.base * 255.0);
            let base = Color::from_rgba8(grey, grey, grey, 255);
            scene.fill(Fill::NonZero, transform, base, None, &board);
            self.draw_plain(art, mask, options, scene, (transform, text));
        }
        let mut at = 0;
        while let Some(base) = drawn.get(at) {
            // The layers clipped to `base`: those directly above it that clip.
            let mut end = at.saturating_add(1);
            while drawn.get(end).is_some_and(|layer| layer.clip) {
                end = end.saturating_add(1);
            }
            let clipped = drawn.get(at.saturating_add(1)..end).unwrap_or_default();
            at = end;
            if !base.visible {
                // What is clipped to a hidden layer is hidden with it.
                continue;
            }
            if !shown(base) {
                // Shown alone, a clipped layer is shown whole.
                for layer in clipped.iter().filter(|layer| layer.visible && shown(layer)) {
                    self.draw_layer(art, layer, options, scene, (transform, text));
                }
                continue;
            }
            let over: Vec<&Layer> = clipped
                .iter()
                .copied()
                .filter(|layer| layer.visible && shown(layer))
                .collect();
            if over.is_empty() {
                self.draw_layer(art, base, options, scene, (transform, text));
                continue;
            }
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &board);
            self.draw_layer(art, base, options, scene, (transform, text));
            for layer in over {
                // Paint only lands where the layer below already has some.
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::new(Mix::Normal, Compose::SrcAtop),
                    1.0,
                    transform,
                    &board,
                );
                self.draw_layer(art, layer, options, scene, (transform, text));
                scene.pop_layer();
            }
            scene.pop_layer();
        }
        scene.pop_layer();
        self.brushes.trim();
        if options.reference
            && let Some(reference) = &art.reference
        {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                f32_of_f64(reference.opacity),
                transform,
                &board,
            );
            self.draw_reference(art, scene, transform);
            scene.pop_layer();
        }
        if options.selection
            && let Some(selection) = &art.selection
        {
            let path = transform * BezPath::from_svg(&selection.d).unwrap_or_default();
            selection_outline(scene, &path, 1.5, 0.0);
        }
        if options.grid {
            draw_grid(art, options.view.unwrap_or(board), scene, transform, text);
        }
        self.tiles.end();
    }

    /// Whether a close view's finer tiles are still being simulated, so the
    /// view should be drawn again soon.
    pub fn detail_pending(&self) -> bool {
        self.tiles.pending()
    }

    /// Whether a painted layer's simulation is being brought up to its
    /// marks in the background, so the view should be drawn again soon.
    pub fn settling(&self) -> bool {
        self.painted.values().any(|entry| entry.settle.running())
    }

    /// Draw one layer, through its mask when it has one switched on.
    fn draw_layer(
        &mut self,
        art: &Artwork,
        layer: &Layer,
        options: &Options,
        scene: &mut Scene,
        (transform, text): (Affine, &mut TextRenderer),
    ) {
        let board = art.bounds();
        let mask = art.mask_of(&layer.id).filter(|mask| mask.visible);
        if mask.is_some() {
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &board);
        }
        if let Some(medium) =
            media::resolve(art, &layer.medium).filter(|_| !layer.medium.is_empty())
        {
            if options.marks.is_none() {
                let wet = options.wet.as_ref().filter(|wet| wet.layer == layer.id);
                self.draw_painted(
                    art,
                    (layer, wet, options.live),
                    &medium,
                    (scene, transform, options.detail.as_ref()),
                    text,
                );
            }
        } else {
            self.draw_plain(art, layer, options, scene, (transform, text));
        }
        if let Some(mask) = mask {
            // The mask's lightness is how much of the layer shows.
            scene.push_luminance_mask_layer(Fill::NonZero, 1.0, transform, &board);
            let grey = byte_of(mask.base * 255.0);
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(grey, grey, grey, 255),
                None,
                &board,
            );
            let plain = Options {
                marks: None,
                ..options.clone()
            };
            self.draw_plain(art, mask, &plain, scene, (transform, text));
            if mask.invert {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::new(Mix::Difference, Compose::SrcOver),
                    1.0,
                    transform,
                    &board,
                );
                scene.fill(Fill::NonZero, transform, MASK_SHOWN, None, &board);
                scene.pop_layer();
            }
            scene.pop_layer();
            scene.pop_layer();
        }
    }

    /// Draw a plain layer's marks as vector art.
    fn draw_plain(
        &mut self,
        art: &Artwork,
        layer: &Layer,
        options: &Options,
        scene: &mut Scene,
        (transform, text): (Affine, &mut TextRenderer),
    ) {
        let board = art.bounds();
        let palette = media::palette_for(art, &layer.id);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            f32_of_f64(layer.opacity),
            transform,
            &board,
        );
        let field = Arc::clone(&self.ground(art).1);
        let tooth = (layer.texture > 0.0)
            .then(|| Speckle::new(self.tooth(art), 1.0 / cell_scale(art), layer.texture));
        let mut cx = MarkCx {
            transform,
            text: &mut *text,
            tooth: tooth.as_ref(),
            brushes: &mut self.brushes,
            paths: &mut self.paths,
            ground: (&field, cell_scale(art)),
            pictures: &mut self.pictures,
            layer_adjust: layer.adjust.as_ref(),
        };
        // A stroke still being laid shows on its layer as it will be
        // drawn: an eraser clears what is under it.
        let wet = options.wet.as_ref().filter(|wet| wet.layer == layer.id);
        for mark in art
            .marks_of(&layer.id)
            .filter(|mark| {
                options
                    .marks
                    .as_ref()
                    .is_none_or(|only| only.contains(&mark.id))
            })
            .chain(wet)
        {
            draw_mark(art, mark, &palette, scene, &mut cx);
        }
        if let Some(amount) = layer.adjust.as_ref().and_then(|adjust| adjust.vignette) {
            vignette(scene, transform, board, amount);
        }
        scene.pop_layer();
    }

    /// The strokes of `art`'s plain layers laid in dabs (pencil, charcoal,
    /// airbrush, a soft eraser), each as a PNG `scale` pixels to a unit and
    /// the area it covers in its mark's units, by id: what an SVG export
    /// embeds.
    pub fn dabbed_pictures(
        &mut self,
        art: &Artwork,
        scale: f64,
    ) -> std::collections::BTreeMap<String, (Vec<u8>, Rect)> {
        let field = Arc::clone(&self.ground(art).1);
        let cells = cell_scale(art);
        let tooth = |at: Point| tooth_at(&field, cells, at);
        let mut pictures = std::collections::BTreeMap::new();
        for mark in &art.marks {
            let plain = art
                .layer(&mark.layer)
                .is_some_and(|layer| layer.medium.is_empty());
            if crate::brushes::stroke_of(mark).is_none() {
                continue;
            }
            let preset = crate::brushes::Preset::of(&mark.style);
            if !plain || !preset.dabbed(&crate::brushes::Dynamics::of_mark(mark)) {
                continue;
            }
            let palette = media::palette_for(art, &mark.layer);
            let ink = if mark.erases() {
                Some(ERASED)
            } else {
                mark.style
                    .stroke
                    .as_deref()
                    .or(mark.style.fill.as_deref())
                    .and_then(|text| colour(text, &palette))
            };
            let Some(ink) = ink.map(Color::to_rgba8) else {
                continue;
            };
            let Some(dabs) = crate::brushes::dab(&crate::brushes::DabInput {
                pieces: &crate::brushes::pieces(mark),
                style: &mark.style,
                scale,
                rgb: [ink.r, ink.g, ink.b],
                tooth: &tooth,
                to_artwork: mark.affine(),
            }) else {
                continue;
            };
            let Ok(png) = crate::view_ops::encode_png(&dabs.pixels, dabs.width, dabs.height) else {
                continue;
            };
            let area = Rect::from_origin_size(
                dabs.origin,
                (
                    f64::from(dabs.width) / dabs.scale,
                    f64::from(dabs.height) / dabs.scale,
                ),
            );
            pictures.insert(mark.id.clone(), (png, area));
        }
        pictures
    }

    /// The reference picture, decoded once per picture.
    pub fn reference_image(&mut self, art: &Artwork) -> Option<ImageData> {
        let address = &art.reference.as_ref()?.image;
        self.pictures.full(address)
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

    fn draw_ground(
        &mut self,
        art: &Artwork,
        scene: &mut Scene,
        transform: Affine,
        detail: Option<&Detail>,
    ) {
        let board = art.bounds();
        let base = colour(&art.ground.colour, &[]).unwrap_or(BLANK_GROUND);
        let texture = Texture::parse(&art.ground.texture).unwrap_or_default();
        if texture == Texture::Smooth {
            scene.fill(Fill::NonZero, transform, base, None, &board);
            return;
        }
        let image = self.ground(art).2.clone();
        let scale = cell_scale(art);
        let halvings = detail.map_or(0, |detail| detail::mip(scale, detail.pixels_per_unit));
        let (image, factor) = self.ground_mips.get(&image, halvings);
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(image),
            Some(Affine::scale(factor / scale)),
            &board,
        );
        // Closer, the tooth is lit again from the ground itself, finer: it
        // is opaque, so it simply covers the coarse ground where it is ready.
        let Some(detail) = detail else {
            return;
        };
        let Some(finer) = detail::level(scale, detail.pixels_per_unit) else {
            return;
        };
        let lit = tilo_wet_media::parse_paint(&art.ground.colour, &[])
            .map_or(tilo_wet_media::Rgb::WHITE, |paint| paint.colour);
        let mut key = blake3::Hasher::new();
        key.update(b"ground");
        key.update(texture.name().as_bytes());
        key.update(art.ground.colour.as_bytes());
        key.update(&finer.to_le_bytes());
        let level = LayerLevel::new(key.finalize(), finer, Vec::new());
        let request = Request {
            slot: "ground",
            work: Work::Ground {
                texture,
                colour: lit,
            },
            level: &level,
            grid: level_grid(art, finer),
        };
        if let Some(mosaic) = self.tiles.mosaic(&request, detail) {
            scene.fill(
                Fill::NonZero,
                transform,
                &ImageBrush::new(mosaic.image),
                Some(mosaic.to_units),
                &mosaic.rect.intersect(board),
            );
        }
    }

    /// The ground's tooth as light and shade a plain layer's marks are
    /// speckled with; paper's when the ground is smooth.
    fn tooth(&mut self, art: &Artwork) -> ImageData {
        let scale = cell_scale(art);
        let (width, height) = (cells(art.width, scale), cells(art.height, scale));
        let texture = match Texture::parse(&art.ground.texture).unwrap_or_default() {
            Texture::Smooth => Texture::Paper,
            texture => texture,
        };
        let key = format!("{}:{width}x{height}", texture.name());
        if let Some((have, image)) = &self.tooth
            && *have == key
        {
            return image.clone();
        }
        let field = Heightfield::new(width, height, texture, f32_of_f64(scale), 1);
        let shaded = field.shade(tilo_wet_media::Rgb::WHITE);
        let relief: Vec<u8> = shaded
            .chunks_exact(4)
            .flat_map(|pixel| {
                // White-lit relief sits just around white; how far from it
                // a cell is says how much light or shade it adds.
                let lit = f64::from(pixel.first().copied().unwrap_or(255));
                speck(byte_of(128.0 + (lit - 236.0) * 3.0))
            })
            .collect();
        let image = crate::picture::image_of(relief, width, height);
        self.tooth = Some((key, image.clone()));
        image
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
            let field = Arc::new(Heightfield::new(
                width,
                height,
                texture,
                f32_of_f64(scale),
                1,
            ));
            let lit = tilo_wet_media::parse_paint(&art.ground.colour, &[])
                .map_or(tilo_wet_media::Rgb::WHITE, |paint| paint.colour);
            let image = crate::picture::image_of(field.shade(lit), width, height);
            self.ground = Some((key, field, image));
        }
        #[allow(clippy::expect_used, reason = "set just above when absent")]
        self.ground.as_ref().expect("ground built")
    }

    fn draw_painted(
        &mut self,
        art: &Artwork,
        (layer, wet, live): (&Layer, Option<&Mark>, bool),
        medium: &media::MediumSpec,
        (scene, transform, detail): (&mut Scene, Affine, Option<&Detail>),
        text: &mut TextRenderer,
    ) {
        let Some(engine) = Engines::builtin().get(&medium.engine) else {
            return;
        };
        let scale = cell_scale(art);
        let ground = Arc::clone(&self.ground(art).1);
        let canvas = Canvas::whole(f32_of_f64(scale), ground);
        let preset = medium.to_preset();
        let settings = Settings::new(engine.parameters(), &preset.params);
        let palette = preset.palette;
        let wait = !live || detail.is_some_and(|detail| detail.wait);
        let (key, shown) = self.painting(
            art,
            (layer, wet, wait),
            (&engine, &canvas, &settings, &palette),
        );
        let blend = match engine.blend() {
            Blend::Normal => BlendMode::default(),
            Blend::Multiply => BlendMode::new(Mix::Multiply, Compose::SrcOver),
        };
        let board = art.bounds();
        scene.push_layer(
            Fill::NonZero,
            blend,
            f32_of_f64(layer.opacity),
            transform,
            &board,
        );
        // Pictures on a painted layer lie under its paint.
        unpainted(
            art,
            layer,
            &palette,
            scene,
            (transform, text, &mut self.pictures),
            true,
        );
        // Closer than the layer is simulated, the part shown is simulated
        // finer; a stroke still being painted shows coarse until it is laid.
        let mosaic = detail
            .filter(|_| wet.is_none())
            .and_then(|detail| self.finer(art, layer, (&engine, &settings, &palette, key), detail));
        self.draw_layer_picture(
            (art, layer, wet.is_some()),
            (shown, mosaic),
            (scene, transform, detail),
        );
        // Lettering is not paint: it stays crisp on a painted layer.
        unpainted(
            art,
            layer,
            &palette,
            scene,
            (transform, text, &mut self.pictures),
            false,
        );
        if let Some(amount) = layer.adjust.as_ref().and_then(|adjust| adjust.vignette) {
            vignette(scene, transform, board, amount);
        }
        scene.pop_layer();
    }

    /// `layer`'s painting as shown now, with `wet` being painted over it,
    /// and the key of its marks and medium. With `wait`, the exact
    /// painting; otherwise the last one finished while the rest is
    /// simulated in the background.
    fn painting(
        &mut self,
        art: &Artwork,
        (layer, wet, wait): (&Layer, Option<&Mark>, bool),
        (engine, canvas, settings, palette): (
            &Arc<dyn tilo_wet_media::MediumEngine>,
            &Canvas,
            &Settings,
            &[Pigment],
        ),
    ) -> (blake3::Hash, Option<ImageData>) {
        let scale = cell_scale(art);
        let entry = self
            .painted
            .entry(layer.id.clone())
            .or_insert_with(|| PaintedLayer {
                settle: Settle::new(LayerPainter::new(
                    Arc::clone(engine),
                    canvas.clone(),
                    settings.clone(),
                )),
                sources: Sources::default(),
                graded: None,
                mips: Mips::default(),
                level: None,
                graded_mosaic: None,
            });
        let (marks_key, marks) = entry.sources.of(art, &layer.id, palette, scale);
        let mut key = blake3::Hasher::new();
        key.update(marks_key.as_bytes());
        key.update(engine.id().as_bytes());
        key.update(format!("{:?}", settings.resolved()).as_bytes());
        let key = key.finalize();
        // What the background finished is taken in first, so the painter
        // is checked before it paints again.
        entry.settle.collect(wait);
        if entry
            .settle
            .painter()
            .is_some_and(|painter| !painter.matches(engine.id(), canvas, settings))
        {
            entry.settle.replace(LayerPainter::new(
                Arc::clone(engine),
                canvas.clone(),
                settings.clone(),
            ));
        }
        let stroke = wet.map(|wet| {
            let strokes = wet_marks(wet, wet_cut(art, wet).as_ref(), palette, scale);
            let mut stroke = blake3::Hasher::new();
            for mark in &strokes {
                stroke.update(&mark.fingerprint);
            }
            (stroke.finalize(), Arc::from(strokes))
        });
        let ask = Ask {
            key,
            marks,
            wet: stroke,
            size: (canvas.width, canvas.height),
        };
        let painting = entry.settle.show(&ask, wait);
        // A stroke being painted shows as it is laid, before the layer's
        // adjustments grade it.
        let shown = match painting {
            Some(painting) if wet.is_none() => Some(graded_painting(
                entry,
                painting,
                layer.adjust.as_ref(),
                (canvas.width, canvas.height),
                scale,
            )),
            painting => painting,
        };
        (key, shown)
    }

    /// The tiles of `layer`'s part shown, simulated at a finer level than
    /// the whole layer; `None` when the view is not that close.
    fn finer(
        &mut self,
        art: &Artwork,
        layer: &Layer,
        (engine, settings, palette, key): (
            &Arc<dyn tilo_wet_media::MediumEngine>,
            &Settings,
            &[Pigment],
            blake3::Hash,
        ),
        detail: &Detail,
    ) -> Option<detail::Mosaic> {
        let finer = detail::level(cell_scale(art), detail.pixels_per_unit)?;
        let mut level_key = blake3::Hasher::new();
        level_key.update(key.as_bytes());
        level_key.update(art.ground.texture.as_bytes());
        level_key.update(&finer.to_le_bytes());
        let level_key = level_key.finalize();
        let entry = self.painted.get_mut(&layer.id)?;
        if entry
            .level
            .as_ref()
            .is_none_or(|level| level.key != level_key)
        {
            let marks = art
                .marks_of(&layer.id)
                .flat_map(|mark| wet_marks(mark, wet_cut(art, mark).as_ref(), palette, finer))
                .collect();
            entry.level = Some(LayerLevel::new(level_key, finer, marks));
        }
        let level = entry.level.as_ref()?;
        let request = Request {
            slot: &layer.id,
            work: Work::Paint {
                engine: Arc::clone(engine),
                settings: settings.clone(),
                texture: Texture::parse(&art.ground.texture).unwrap_or_default(),
                marks: Arc::clone(&level.marks),
                margin: level.margin,
            },
            level,
            grid: level_grid(art, finer),
        };
        self.tiles.mosaic(&request, detail)
    }

    /// A painted layer's picture: the finer tiles where they are ready, the
    /// whole layer's coarser picture (or a halving of it, from afar)
    /// elsewhere.
    fn draw_layer_picture(
        &mut self,
        (art, layer, wet): (&Artwork, &Layer, bool),
        (shown, mosaic): (Option<ImageData>, Option<detail::Mosaic>),
        (scene, transform, detail): (&mut Scene, Affine, Option<&Detail>),
    ) {
        let board = art.bounds();
        let scale = cell_scale(art);
        if let Some(image) = shown {
            let halvings = match (detail, &mosaic) {
                (Some(detail), None) => detail::mip(scale, detail.pixels_per_unit),
                _ => 0,
            };
            let (image, factor) = match self.painted.get_mut(&layer.id) {
                Some(entry) if !wet => entry.mips.get(&image, halvings),
                _ => (image, 1.0),
            };
            // Where the finer tiles are ready, they alone show the paint.
            let area = match &mosaic {
                Some(mosaic) => {
                    let mut holes = BezPath::new();
                    for hole in &mosaic.missing {
                        holes.extend(hole.intersect(board).path_elements(0.1));
                    }
                    holes
                }
                None => board.to_path(0.1),
            };
            if !area.elements().is_empty() {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    &ImageBrush::new(image),
                    Some(Affine::scale(factor / scale)),
                    &area,
                );
            }
        }
        if let Some(mosaic) = mosaic {
            let image = self.graded_mosaic(layer, &mosaic);
            scene.fill(
                Fill::NonZero,
                transform,
                &ImageBrush::new(image),
                Some(mosaic.to_units),
                &mosaic.rect.intersect(board),
            );
        }
    }

    /// The finer tiles as the layer's adjustments show them, graded once
    /// per picture of them and adjustment.
    fn graded_mosaic(&mut self, layer: &Layer, mosaic: &detail::Mosaic) -> ImageData {
        let image = mosaic.image.clone();
        let (Some(adjust), Some(entry)) = (layer.adjust.as_ref(), self.painted.get_mut(&layer.id))
        else {
            return image;
        };
        let Some(finer) = entry.level.as_ref().map(|level| level.scale) else {
            return image;
        };
        let treatment = Treatment::of(&[adjust], finer);
        if treatment.is_empty() {
            return image;
        }
        if let Some((have, key, graded)) = &entry.graded_mosaic
            && *have == image.data.id()
            && *key == treatment.key
        {
            return graded.clone();
        }
        let (width, height) = (image.width as usize, image.height as usize);
        let mut pixels = image.data.data().to_vec();
        treatment.run(&mut pixels, width, height, 1.0);
        let graded = crate::picture::image_of(pixels, width, height);
        entry.graded_mosaic = Some((image.data.id(), treatment.key, graded.clone()));
        graded
    }
}

/// The whole grid of a painted layer at `scale` cells per unit.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "artwork sides are positive and bounded"
)]
fn level_grid(art: &Artwork, scale: f64) -> (usize, usize) {
    let side = |units: f64| (units * scale).round().max(1.0) as usize;
    (side(art.width), side(art.height))
}

/// The marks of a painted layer its medium does not paint: its pictures
/// (`pictures`), or its lettering, graded by their own and the layer's
/// adjustments.
fn unpainted(
    art: &Artwork,
    layer: &Layer,
    palette: &[Pigment],
    scene: &mut Scene,
    (transform, text, store): (Affine, &mut TextRenderer, &mut Pictures),
    pictures: bool,
) {
    for mark in art.marks_of(&layer.id) {
        match &mark.shape {
            Shape::Image { .. } if pictures => {
                draw_picture(mark, layer.adjust.as_ref(), scene, transform, store);
            }
            Shape::Text { .. } if !pictures => {
                let mark = graded_mark(
                    mark,
                    palette,
                    &grades_of(&[mark.style.adjust.as_ref(), layer.adjust.as_ref()]),
                );
                draw_vector(&mark, palette, scene, transform, text);
            }
            _ => {}
        }
    }
}

/// The per-pixel grades of `adjusts`, those given, in order.
pub(crate) fn grades_of(adjusts: &[Option<&Adjust>]) -> Vec<Grade> {
    adjusts
        .iter()
        .flatten()
        .filter_map(|adjust| adjust.grade())
        .collect()
}

/// `mark` with its colours graded by `grades`, as a grade of the pixels
/// would show a flat colour: fill, outline, gradient stops and shadows.
pub(crate) fn graded_mark(mark: &Mark, palette: &[Pigment], grades: &[Grade]) -> Mark {
    let mut mark = mark.clone();
    if grades.is_empty() {
        return mark;
    }
    let grade = |text: &mut String| {
        if let Some(named) = colour(text, palette) {
            let rgba = named.to_rgba8();
            let mut channels = [rgba.r, rgba.g, rgba.b, rgba.a];
            for grade in grades {
                channels = grade.colour(channels);
            }
            let [r, g, b, a] = channels;
            *text = format!("#{r:02x}{g:02x}{b:02x}{a:02x}");
        }
    };
    let style = &mut mark.style;
    for text in [&mut style.fill, &mut style.stroke].into_iter().flatten() {
        grade(text);
    }
    if let Some(gradient) = &mut style.gradient {
        for (_, stop) in &mut gradient.stops {
            grade(stop);
        }
    }
    for shadow in [&mut style.shadow, &mut style.inner_shadow]
        .into_iter()
        .flatten()
    {
        if let Some(ink) = &mut shadow.colour {
            grade(ink);
        }
    }
    mark
}

/// A painted layer's painting as its adjustments show it, graded once per
/// painting and adjustment.
fn graded_painting(
    entry: &mut PaintedLayer,
    image: ImageData,
    adjust: Option<&Adjust>,
    (width, height): (usize, usize),
    cells_per_unit: f64,
) -> ImageData {
    let Some(adjust) = adjust else {
        return image;
    };
    let treatment = Treatment::of(&[adjust], cells_per_unit);
    if treatment.is_empty() {
        return image;
    }
    let key = format!("{}|{}", image.data.id(), treatment.key);
    if let Some((have, graded)) = &entry.graded
        && *have == key
    {
        return graded.clone();
    }
    let mut pixels = image.data.data().to_vec();
    treatment.run(&mut pixels, width, height, 1.0);
    let graded = crate::picture::image_of(pixels, width, height);
    entry.graded = Some((key, graded.clone()));
    graded
}

/// The scale a transform draws at: device pixels per unit.
fn pixels_per_unit(transform: Affine) -> f64 {
    transform.determinant().abs().sqrt()
}

/// Draw an image mark's picture into its frame: cropped, turned, graded by
/// its own adjustments then its layer's, at the resolution it is shown at,
/// with its own vignette over it.
fn draw_picture(
    mark: &Mark,
    layer_adjust: Option<&Adjust>,
    scene: &mut Scene,
    transform: Affine,
    pictures: &mut Pictures,
) {
    let (Shape::Image { src, .. }, Some(placement)) = (&mark.shape, Placement::of(&mark.shape))
    else {
        return;
    };
    let at = transform * mark.affine();
    let frame = placement.frame;
    let Some(size) = pictures.size(src) else {
        // The picture's bytes are not here (yet): its frame stays visible,
        // shaded as a shadow is.
        scene.fill(Fill::NonZero, at, SHADOW_INK, None, &frame);
        return;
    };
    let per_unit = placement.pixels_per_unit(size);
    let adjusts: Vec<&Adjust> = [mark.style.adjust.as_ref(), layer_adjust]
        .into_iter()
        .flatten()
        .collect();
    let treatment = Treatment::of(&adjusts, per_unit);
    let device = pixels_per_unit(at) / per_unit.max(1e-9);
    let Some((image, step)) = pictures.shown(
        src,
        if mark.style.pixelated {
            device.max(1.0)
        } else {
            device
        },
        &treatment,
    ) else {
        return;
    };
    let mut brush = ImageBrush::new(image).with_alpha(f32_of_f64(mark.style.opacity()));
    if mark.style.pixelated {
        brush = brush.with_quality(ui::vello::peniko::ImageQuality::Low);
    }
    let to_frame = placement.picture_to_local(size) * Affine::scale(step);
    let vignetted = mark
        .style
        .adjust
        .as_ref()
        .and_then(|adjust| adjust.vignette);
    if vignetted.is_some() {
        // The vignette lands only on the picture: both in a group of their
        // own.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, at, &frame);
    }
    scene.fill(Fill::NonZero, at, &brush, Some(to_frame), &frame);
    if let Some(amount) = vignetted {
        vignette(scene, at, frame, amount);
        scene.pop_layer();
    }
}

/// A vignette over what is already drawn in the current layer within
/// `area`: black (or, below zero, white) growing toward its edges, only
/// where something is drawn.
fn vignette(scene: &mut Scene, transform: Affine, area: Rect, amount: f64) {
    if amount == 0.0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return;
    }
    // The artwork's own inks, whatever the theme: its shadows' black
    // darkens, its blank ground's white lightens.
    let ink = if amount > 0.0 {
        SHADOW_INK.with_alpha(1.0)
    } else {
        BLANK_GROUND
    };
    let reach = crate::adjust::VIGNETTE_REACH;
    let stops: Vec<ColorStop> = (0..=12)
        .map(|step| {
            let distance = reach * f64::from(step) / 12.0;
            ColorStop {
                offset: f32_of_f64(distance / reach),
                color: ink
                    .with_alpha(f32_of_f64(crate::adjust::vignette_alpha(amount, distance)))
                    .into(),
            }
        })
        .collect();
    let gradient = PenikoGradient::new_radial((0.0, 0.0), f32_of_f64(reach))
        .with_extend(Extend::Pad)
        .with_stops(&stops[..]);
    // The unit circle stretched over the area: 1 at the middle of each edge.
    let to_area = Affine::translate(area.center().to_vec2())
        * Affine::scale_non_uniform(area.width() / 2.0, area.height() / 2.0);
    scene.push_layer(
        Fill::NonZero,
        BlendMode::new(Mix::Normal, Compose::SrcAtop),
        1.0,
        transform,
        &area,
    );
    scene.fill(Fill::NonZero, transform, &gradient, Some(to_area), &area);
    scene.pop_layer();
}

/// A mask's full lightness, where all of its layer shows: a value of the
/// artwork's data, the white of a blank ground, not a colour of the theme.
const MASK_SHOWN: Color = BLANK_GROUND;

/// A selection's outline, `path` already where it is drawn: black dashes
/// over white, `width` wide, the dashes `phase` along (marching ants). The
/// selection's colours, not the theme's: it is drawn over the artwork.
///
/// Light under dark dashes reads over any artwork, in every theme: the
/// white of a blank ground and the black of a shadow, made solid.
pub fn selection_outline(scene: &mut Scene, path: &BezPath, width: f64, phase: f64) {
    let under = Stroke::new(width * 1.5);
    let dashes = Stroke::new(width).with_dashes(phase, [4.0 * width, 4.0 * width]);
    scene.stroke(&under, Affine::IDENTITY, BLANK_GROUND, None, path);
    scene.stroke(
        &dashes,
        Affine::IDENTITY,
        SHADOW_INK.with_alpha(1.0),
        None,
        path,
    );
}

/// The outline of `path` flattened into polygons, in cell coordinates.
fn contours(path: &BezPath, to_cells: Affine) -> Vec<Vec<(f32, f32)>> {
    let mut contours: Vec<Vec<(f32, f32)>> = Vec::new();
    ui::vello::kurbo::flatten(to_cells * path.clone(), 0.25, |element| match element {
        PathEl::MoveTo(at) => contours.push(vec![(f32_of_f64(at.x), f32_of_f64(at.y))]),
        PathEl::LineTo(at) => {
            if let Some(contour) = contours.last_mut() {
                contour.push((f32_of_f64(at.x), f32_of_f64(at.y)));
            }
        }
        _ => {}
    });
    contours.retain(|contour| contour.len() >= 2);
    contours
}

/// The outline a stroke's paint is cut to, in cells: its clip's silhouette.
fn clip_contours(cut: Option<&BezPath>, scale: f64) -> Vec<Vec<(f32, f32)>> {
    cut.map(|outline| contours(outline, Affine::scale(scale)))
        .unwrap_or_default()
}

/// The outline of what a mark is cut to, in artwork units: its clip's
/// silhouette, or every member's of a clipping group.
fn clip_path(clips: &[&Mark]) -> BezPath {
    let mut path = BezPath::new();
    for clip in clips {
        let mut outline = silhouette(clip);
        outline.apply_affine(clip.affine());
        path.extend(outline);
    }
    path
}

/// The outlines `mark` is cut to, in artwork units: what its clip names
/// and the area it is kept inside. It shows only where all of them cover.
pub fn cuts(art: &Artwork, mark: &Mark) -> Vec<BezPath> {
    let clips = art.clips_of(mark);
    (!clips.is_empty())
        .then(|| clip_path(&clips))
        .into_iter()
        .chain(mark.area())
        .collect()
}

/// The one outline a painted mark is kept inside (the medium keeps a
/// stroke inside one): its area, else what its clip names.
fn wet_cut(art: &Artwork, mark: &Mark) -> Option<BezPath> {
    cuts(art, mark).pop()
}

/// A stroke's points in cells, with their pressure.
fn cell_points(points: &[Vec<f64>], to_cells: Affine) -> Vec<WetPoint> {
    points
        .iter()
        .map(|point| {
            let at = to_cells
                * Point::new(
                    point.first().copied().unwrap_or(0.0),
                    point.get(1).copied().unwrap_or(0.0),
                );
            WetPoint::new(
                f32_of_f64(at.x),
                f32_of_f64(at.y),
                f32_of_f64(point.get(2).copied().unwrap_or(1.0)),
            )
        })
        .collect()
}

/// What a mark is as paint: a laid-in region, a stroke, or both. A stroke
/// stays inside `clip` when it is cut to one.
fn wet_marks(mark: &Mark, clip: Option<&BezPath>, palette: &[Pigment], scale: f64) -> Vec<Painted> {
    let style = &mark.style;
    let to_cells = Affine::scale(scale) * mark.affine();
    let params = style
        .params
        .iter()
        .map(|(id, value)| (id.clone(), f32_of_f64(*value)))
        .collect();
    let opacity = f32_of_f64(style.opacity());
    let paint = |text: &Option<String>| {
        text.as_deref()
            .and_then(|text| tilo_wet_media::parse_paint(text, palette))
    };
    let mut painted = Vec::new();
    match &mark.shape {
        Shape::Stroke { points, size, .. } if crate::brushes::Preset::of(style).erases() => {
            let dynamics = crate::brushes::Dynamics::of_mark(mark);
            painted.push(Painted::new(
                &mark.id,
                WetMark::Erase(tilo_wet_media::Erase {
                    points: cell_points(points, to_cells),
                    size: f32_of_f64(size * scale),
                    strength: opacity,
                    hardness: f32_of_f64(dynamics.hardness),
                    clip: clip_contours(clip, scale),
                }),
            ));
        }
        Shape::Stroke { points, size, .. } => {
            if let Some(colour) = paint(&style.stroke).or_else(|| paint(&style.fill)) {
                let points = cell_points(points, to_cells);
                painted.push(Painted::new(
                    &mark.id,
                    WetMark::Stroke(WetStroke {
                        points,
                        size: f32_of_f64(size * scale),
                        paint: colour,
                        opacity,
                        wet: style.wet,
                        params,
                        form: mark
                            .shape
                            .form(style.angle)
                            .map(|form| form.scaled(f32_of_f64(scale))),
                        clip: clip_contours(clip, scale),
                    }),
                ));
            }
        }
        Shape::Text { .. } | Shape::Image { .. } => {}
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
                let brush = style.brush_size.unwrap_or_else(|| {
                    (bounds.width().min(bounds.height()) / 5.0).clamp(6.0, 48.0)
                });
                painted.push(Painted::new(
                    &mark.id,
                    WetMark::Fill(WetFill {
                        contours: contours(&path, to_cells),
                        paint: colour,
                        opacity,
                        wet: style.wet,
                        brush: f32_of_f64(brush * scale),
                        angle: f32_of_f64(style.angle.unwrap_or(0.0)),
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
            Some((f32_of_f64(*at), paint.colour, paint.alpha))
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
        (f32_of_f64(from.x), f32_of_f64(from.y)),
        (f32_of_f64(to.x), f32_of_f64(to.y)),
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
                    size: f32_of_f64(mark.style.stroke_width.max(1.0) * scale),
                    paint: colour,
                    opacity: f32_of_f64(mark.style.opacity()),
                    wet: true,
                    params: params.clone(),
                    form: None,
                    clip: Vec::new(),
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
                offset: f32_of_f64(at.clamp(0.0, 1.0)),
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
        let radius = f32_of_f64(from.distance(to));
        match gradient.focal {
            Some([x, y]) => PenikoGradient::new_two_point_radial((x, y), 0.0, from, radius),
            None => PenikoGradient::new_radial(from, radius),
        }
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

/// The region a mark covers, untransformed: a brush stroke's inked
/// outline as its preset lays it, a text's box, any other shape's outline.
pub fn silhouette(mark: &Mark) -> BezPath {
    match &mark.shape {
        Shape::Stroke { .. }
            if crate::brushes::Preset::of(&mark.style) == crate::brushes::Preset::Round =>
        {
            ink_strokes(mark)
        }
        Shape::Stroke { .. } => {
            crate::brushes::silhouette(&crate::brushes::pieces(mark), &mark.style)
        }
        shape => shape.path(),
    }
}

/// A stroke drawn by the round ink brush: its own line, or every twig or
/// dab it grows into, each after the stroke's smoothing.
fn ink_strokes(mark: &Mark) -> BezPath {
    let smoothing = crate::brushes::Dynamics::of_mark(mark).smoothing;
    let mut path = BezPath::new();
    for (points, size) in crate::brushes::pieces(mark) {
        // Laid as every brush lays its path, so its corners stay sharp.
        let points = crate::brushes::steady_ends(&points, size);
        let samples = crate::brushes::samples(&points, 0.75, smoothing, size);
        path.extend(
            ink_outline(&samples, f32_of_f64(size))
                .elements()
                .iter()
                .copied(),
        );
    }
    path
}

fn ink_outline(samples: &[tilo_wet_media::brush::Sample], size: f32) -> BezPath {
    let outline = tilo_wet_media::brush::ink_outline(samples, size);
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
    cx: &mut MarkCx<'_>,
) {
    let transform = cx.transform;
    let preset = crate::brushes::Preset::of(&mark.style);
    if matches!(mark.shape, Shape::Stroke { .. }) && preset.erases() {
        draw_eraser(art, mark, scene, cx);
        return;
    }
    let picture = matches!(mark.shape, Shape::Image { .. });
    let graded;
    let adjustments = grades_of(&[mark.style.adjust.as_ref(), cx.layer_adjust]);
    let mark = if picture || adjustments.is_empty() {
        mark
    } else {
        graded = graded_mark(mark, palette, &adjustments);
        &graded
    };
    let mut layers = 0;
    for outline in cuts(art, mark) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &outline,
        );
        layers += 1;
    }
    if let Some(mix) = mark
        .style
        .blend
        .as_deref()
        .or_else(|| crate::brushes::default_blend(&mark.style))
        .and_then(mix)
    {
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
        draw_shadow(mark, shadow, palette, scene, (transform, &mut *cx.text));
    }
    let style = &mark.style;
    let mut speckles: Vec<Speckle> = Vec::new();
    if let Some(grain) = style.grain.filter(|grain| *grain > 0.0) {
        speckles.push(Speckle::grain(grain));
    }
    speckles.extend(cx.tooth.cloned());
    if matches!(mark.shape, Shape::Text { .. }) {
        // Lettering keeps its clean edges.
        speckles.clear();
    }
    // Speckle laid over paint that is partly clear must not show where the
    // paint does not: the mark is drawn into a group of its own and the
    // speckle lands only on what it covers. Opaque paint takes the cheap
    // path, its outline filled with the speckle.
    let isolated = !speckles.is_empty() && !opaque(mark, palette);
    let reach = if isolated {
        mark.bounds().inflate(
            style.feather.unwrap_or(0.0) + 2.0,
            style.feather.unwrap_or(0.0) + 2.0,
        )
    } else {
        Rect::ZERO
    };
    let path = cx.paths.get(mark);
    if isolated {
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &reach);
    }
    let dabbed = matches!(mark.shape, Shape::Stroke { .. })
        && preset.dabbed(&crate::brushes::Dynamics::of_mark(mark));
    match style.feather {
        _ if picture => {
            draw_picture(mark, cx.layer_adjust, scene, transform, cx.pictures);
            draw_vector_path(mark, palette, scene, transform, cx.text, path.as_deref());
        }
        _ if dabbed => draw_dabbed(mark, palette, scene, cx, Compose::SrcOver),
        Some(feather) if feather > 0.0 => draw_feathered(mark, palette, scene, transform, feather),
        _ => draw_vector_path(mark, palette, scene, transform, cx.text, path.as_deref()),
    }
    if let Some(inner) = &style.inner_shadow {
        draw_inner_shadow(mark, inner, palette, scene, transform);
    }
    for speckle in &speckles {
        if isolated {
            speckle.atop(&reach, scene, transform);
        } else {
            speckle.over(mark, scene, transform);
        }
    }
    if isolated {
        scene.pop_layer();
    }
    for _ in 0..layers {
        scene.pop_layer();
    }
}

/// An eraser on a plain layer: what it covers is taken out of the marks
/// drawn before it on the layer, as much as its opacity says.
fn draw_eraser(art: &Artwork, mark: &Mark, scene: &mut Scene, cx: &mut MarkCx<'_>) {
    let transform = cx.transform;
    scene.push_layer(
        Fill::NonZero,
        BlendMode::new(Mix::Normal, Compose::DestOut),
        1.0,
        transform,
        &art.bounds(),
    );
    // Cut to its clip inside the erasing group, so only what it covers
    // within the clip is taken out.
    let cuts = cuts(art, mark);
    for outline in &cuts {
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, outline);
    }
    if crate::brushes::Preset::of(&mark.style).dabbed(&crate::brushes::Dynamics::of_mark(mark)) {
        draw_dabbed(mark, &[], scene, cx, Compose::SrcOver);
    } else {
        scene.fill(
            Fill::NonZero,
            transform * mark.affine(),
            ERASED.multiply_alpha(f32_of_f64(mark.style.opacity())),
            None,
            &silhouette(mark),
        );
    }
    for _ in &cuts {
        scene.pop_layer();
    }
    scene.pop_layer();
}

/// The ground's height at `at` (artwork units), read between its cells.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "cell coordinates are clamped to the field before the cast"
)]
fn tooth_at(field: &Heightfield, scale: f64, at: Point) -> f64 {
    let x = (at.x * scale - 0.5).clamp(0.0, field.width.saturating_sub(1) as f64);
    let y = (at.y * scale - 0.5).clamp(0.0, field.height.saturating_sub(1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (tx, ty) = (x - x.floor(), y - y.floor());
    let get =
        |dx: usize, dy: usize| f64::from(field.at(x0.saturating_add(dx), y0.saturating_add(dy)));
    let top = get(0, 0) + (get(1, 0) - get(0, 0)) * tx;
    let bottom = get(0, 1) + (get(1, 1) - get(0, 1)) * tx;
    top + (bottom - top) * ty
}

/// A stroke of a dabbed preset (pencil, charcoal, airbrush, a soft
/// eraser): its dabs as pixels fixed to the artwork, laid as finely as it
/// is shown, kept between frames.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a power of two between a quarter and four"
)]
fn draw_dabbed(
    mark: &Mark,
    palette: &[Pigment],
    scene: &mut Scene,
    cx: &mut MarkCx<'_>,
    compose: Compose,
) {
    if crate::brushes::stroke_of(mark).is_none() {
        return;
    }
    let style = &mark.style;
    let eraser = crate::brushes::Preset::of(style).erases();
    let ink = if eraser {
        ERASED
    } else {
        let Some(ink) = style
            .stroke
            .as_deref()
            .or(style.fill.as_deref())
            .and_then(|text| colour(text, palette))
        else {
            return;
        };
        ink
    };
    let to_screen = cx.transform * mark.affine();
    let zoom = to_screen.determinant().abs().sqrt().max(1e-6);
    // Laid as finely as shown, in steps of two so zooming does not lay it
    // again at every frame.
    let scale = 2.0_f64.powi(zoom.log2().ceil().clamp(-2.0, 2.0) as i32);
    let rgba = ink.to_rgba8();
    let key = dab_key(mark, [rgba.r, rgba.g, rgba.b], scale, cx);
    cx.brushes.tick = cx.brushes.tick.saturating_add(1);
    let tick = cx.brushes.tick;
    if !cx.brushes.entries.contains_key(&key) {
        let (field, cells) = cx.ground;
        let tooth = |at: Point| tooth_at(field, cells, at);
        let Some(dabs) = crate::brushes::dab(&crate::brushes::DabInput {
            pieces: &crate::brushes::pieces(mark),
            style,
            scale,
            rgb: [rgba.r, rgba.g, rgba.b],
            tooth: &tooth,
            to_artwork: mark.affine(),
        }) else {
            return;
        };
        let image = ui::raster::rgba8(dabs.pixels, dabs.width, dabs.height);
        cx.brushes.bytes = cx.brushes.bytes.saturating_add(image.data.len());
        cx.brushes.entries.insert(
            key,
            LaidDabs {
                image,
                origin: dabs.origin,
                scale: dabs.scale,
                used: tick,
            },
        );
    }
    let Some(laid) = cx.brushes.entries.get_mut(&key) else {
        return;
    };
    laid.used = tick;
    let alpha = f64::from(ink.components[3]) * style.opacity();
    let area = Rect::from_origin_size(
        laid.origin,
        (
            f64::from(laid.image.width) / laid.scale,
            f64::from(laid.image.height) / laid.scale,
        ),
    );
    let brush = ImageBrush::new(laid.image.clone()).with_alpha(f32_of_f64(alpha));
    let place = Affine::translate(laid.origin.to_vec2()) * Affine::scale(1.0 / laid.scale);
    if compose == Compose::SrcOver {
        scene.fill(Fill::NonZero, to_screen, &brush, Some(place), &area);
    } else {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::new(Mix::Normal, compose),
            1.0,
            to_screen,
            &area,
        );
        scene.fill(Fill::NonZero, to_screen, &brush, Some(place), &area);
        scene.pop_layer();
    }
}

/// What a dabbed stroke's laid pixels depend on: the stroke, its ink, how
/// finely it is laid and the ground it catches on.
fn dab_key(mark: &Mark, rgb: [u8; 3], scale: f64, cx: &MarkCx<'_>) -> [u8; 32] {
    let style = &mark.style;
    let mut key = blake3::Hasher::new();
    key.update(mark.id.as_bytes());
    if let Shape::Stroke {
        points,
        size,
        forks,
        spread,
        dabs,
        scatter,
        dab_length,
        hatch,
    } = &mark.shape
    {
        // Hashed as bytes: this runs for every dabbed stroke every frame.
        for point in points {
            for value in point {
                key.update(&value.to_le_bytes());
            }
            key.update(&[0xff]);
        }
        key.update(&size.to_le_bytes());
        key.update(
            format!("{forks:?}{spread:?}{dabs:?}{scatter:?}{dab_length:?}{hatch:?}").as_bytes(),
        );
    }
    key.update(format!("{:?}{:?}{:?}", style.brush, style.params, style.angle).as_bytes());
    key.update(&rgb);
    key.update(&scale.to_le_bytes());
    key.update(format!("{:?}", mark.transform).as_bytes());
    key.update(&cx.ground.1.to_le_bytes());
    key.update(format!("{:?}", cx.ground.0.texture).as_bytes());
    *key.finalize().as_bytes()
}

/// Whether `mark` paints its whole outline solidly: no clear stop in its
/// gradient, no clear fill, no opacity, no feathered edge.
fn opaque(mark: &Mark, palette: &[Pigment]) -> bool {
    let style = &mark.style;
    let solid =
        |text: &str| colour(text, palette).is_some_and(|colour| colour.components[3] >= 1.0);
    let laid_solid = match &mark.shape {
        Shape::Stroke { .. } => {
            crate::brushes::ink_alpha(style) >= 1.0
                && !crate::brushes::Preset::of(style)
                    .dabbed(&crate::brushes::Dynamics::of_mark(mark))
        }
        _ => true,
    };
    style.opacity() >= 1.0
        && laid_solid
        && style.feather.is_none()
        && (mark.shape.closed() || matches!(mark.shape, Shape::Stroke { .. }))
        && match &style.gradient {
            Some(gradient) => gradient.stops.iter().all(|(_, stop)| solid(stop)),
            None => style
                .fill
                .as_deref()
                .or(style.stroke.as_deref())
                .is_some_and(solid),
        }
}

/// The side of the grain tile, in texels.
const GRAIN_TILE: usize = 128;

/// A grey's departure from mid grey as light (white) or shade (black)
/// whose alpha is how far it departs: laid over paint, it lightens and
/// darkens it without a blend layer.
fn speck(grey: u8) -> [u8; 4] {
    if grey >= 128 {
        [255, 255, 255, grey - 128]
    } else {
        [0, 0, 0, 128 - grey]
    }
}

/// A tile of film grain, the same everywhere.
fn noise() -> ImageData {
    static NOISE: std::sync::OnceLock<ImageData> = std::sync::OnceLock::new();
    NOISE
        .get_or_init(|| {
            let mut pixels = Vec::with_capacity(GRAIN_TILE * GRAIN_TILE * 4);
            let mut state: u32 = 0x9e37_79b9;
            for _ in 0..GRAIN_TILE * GRAIN_TILE {
                // xorshift: fast, fixed, and good enough for grain.
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let [a, b, ..] = state.to_le_bytes();
                // Two draws averaged: grain clusters around mid grey.
                let grey = u8::try_from(u16::midpoint(u16::from(a), u16::from(b))).unwrap_or(128);
                pixels.extend_from_slice(&speck(grey));
            }
            crate::picture::image_of(pixels, GRAIN_TILE, GRAIN_TILE)
        })
        .clone()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to a byte's range before the cast"
)]
fn byte_of(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// A tile of light and shade (see [`speck`]) laid over marks: grain, or the
/// ground's tooth showing through a layer.
#[derive(Clone)]
pub struct Speckle {
    tile: ImageData,
    /// Artwork units per texel.
    unit: f64,
    amount: f64,
}

impl Speckle {
    fn new(tile: ImageData, unit: f64, amount: f64) -> Self {
        Self { tile, unit, amount }
    }

    /// Film grain at `amount`, a texel to an artwork unit.
    fn grain(amount: f64) -> Self {
        Self::new(noise(), 1.0, amount)
    }

    /// The tile, repeated and fixed to the artwork so it does not swim when
    /// a mark moves.
    fn brush(&self, opacity: f64) -> ImageBrush {
        ImageBrush::new(self.tile.clone())
            .with_extend(Extend::Repeat)
            .with_alpha(f32_of_f64((self.amount * opacity).clamp(0.0, 1.0)))
    }

    /// Over the outline of `mark`, which paints it solidly.
    fn over(&self, mark: &Mark, scene: &mut Scene, transform: Affine) {
        let mut path = silhouette(mark);
        path.apply_affine(mark.affine());
        let brush = self.brush(mark.style.opacity());
        scene.fill(
            Fill::NonZero,
            transform,
            &brush,
            Some(Affine::scale(self.unit)),
            &path,
        );
    }

    /// Over whatever is drawn within `reach` of the group it is laid in,
    /// as much as it is drawn.
    fn atop(&self, reach: &Rect, scene: &mut Scene, transform: Affine) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::new(Mix::Normal, Compose::SrcAtop),
            1.0,
            transform,
            reach,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            &self.brush(1.0),
            Some(Affine::scale(self.unit)),
            reach,
        );
        scene.pop_layer();
    }
}

/// A mark whose edge fades out over `feather` units: its paint as drawn
/// inside, melting away outside.
fn draw_feathered(
    mark: &Mark,
    palette: &[Pigment],
    scene: &mut Scene,
    transform: Affine,
    feather: f64,
) {
    let style = &mark.style;
    let closed = mark.shape.closed() || matches!(mark.shape, Shape::Stroke { .. });
    let named = |value: Option<&str>| value.and_then(|value| colour(value, palette));
    let brush: Option<ui::vello::peniko::Brush> = if closed {
        style
            .gradient
            .as_ref()
            .and_then(|gradient| gradient_brush(gradient, palette, 1.0))
            .map(Into::into)
            .or_else(|| named(style.fill.as_deref().or(style.stroke.as_deref())).map(Into::into))
    } else {
        named(style.stroke.as_deref()).map(Into::into)
    };
    let Some(brush) = brush else {
        return;
    };
    let line = if closed {
        0.0
    } else {
        style.stroke_width.max(1.0)
    };
    soft_paint(
        scene,
        transform * mark.affine(),
        &brush,
        &silhouette(mark),
        (closed, line),
        feather,
        style.opacity(),
    );
}

/// A shadow inside `mark` from its edge: the world outside its silhouette,
/// offset and softened, seen only through the mark.
fn draw_inner_shadow(
    mark: &Mark,
    shadow: &crate::model::Shadow,
    palette: &[Pigment],
    scene: &mut Scene,
    transform: Affine,
) {
    if matches!(mark.shape, Shape::Text { .. }) {
        return;
    }
    let ink = shadow
        .colour
        .as_deref()
        .and_then(|value| colour(value, palette))
        .unwrap_or(SHADOW_INK);
    let alpha = f64::from(ink.components[3]) * mark.style.opacity();
    let at = transform * mark.affine();
    let path = silhouette(mark);
    let [dx, dy] = shadow.offset;
    // Everything around the silhouette: a frame well past its edge with the
    // silhouette cut out of it, offset; its soft edge reaches inward.
    let margin = shadow.blur + dx.abs() + dy.abs() + 4.0;
    let around = {
        use ui::vello::kurbo::Shape as _;
        let mut around = path.bounding_box().inflate(margin, margin).to_path(0.1);
        around.extend(path.iter());
        around
    };
    let shifted = at * Affine::translate((dx, dy));
    let rings = if shadow.blur > 0.0 { SHADOW_RINGS } else { 1 };
    let each = 1.0 - (1.0 - alpha.clamp(0.0, 0.999)).powf(1.0 / f64::from(rings));
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, at, &path);
    scene.fill(
        Fill::EvenOdd,
        shifted,
        ink.with_alpha(f32_of_f64(alpha)),
        None,
        &around,
    );
    if shadow.blur > 0.0 {
        // The falloff inside the offset silhouette.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, shifted, &path);
        for ring in 0..rings {
            let grow = shadow.blur * (f64::from(ring) + 0.5) / f64::from(rings);
            let pen = Stroke::new(grow * 2.0).with_join(Join::Round);
            scene.stroke(&pen, shifted, ink.with_alpha(f32_of_f64(each)), None, &path);
        }
        scene.pop_layer();
    }
    scene.pop_layer();
}

/// How many rings a soft shadow or edge is built from.
const SHADOW_RINGS: u16 = 10;

/// `path` painted with `brush` at `alpha`, its edge softened outward over
/// `soft` units: filled once, then rings of outline outside it only, each
/// letting through the share that compounds to `alpha` at the edge. An
/// open line (`filled` false, `line` wide) softens to both sides. One clip
/// layer at most, however many rings: a layer per ring exhausts the
/// renderer's blend memory on a busy artwork.
fn soft_paint(
    scene: &mut Scene,
    at: Affine,
    brush: &ui::vello::peniko::Brush,
    path: &BezPath,
    (filled, line): (bool, f64),
    soft: f64,
    alpha: f64,
) {
    let rings = if soft > 0.0 { SHADOW_RINGS } else { 1 };
    let each = 1.0 - (1.0 - alpha.clamp(0.0, 0.999)).powf(1.0 / f64::from(rings));
    let grow = |ring: u16| soft * (f64::from(ring) + 0.5) / f64::from(rings);
    if !filled {
        for ring in 0..rings {
            let width = line + if soft > 0.0 { grow(ring) * 2.0 } else { 0.0 };
            let pen = Stroke::new(width)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            let faint = brush.clone().multiply_alpha(f32_of_f64(each));
            scene.stroke(&pen, at, &faint, None, path);
        }
        return;
    }
    let solid = brush.clone().multiply_alpha(f32_of_f64(alpha));
    scene.fill(Fill::NonZero, at, &solid, None, path);
    if soft <= 0.0 {
        return;
    }
    let outside = {
        use ui::vello::kurbo::Shape as _;
        let mut outside = path
            .bounding_box()
            .inflate(soft + 4.0, soft + 4.0)
            .to_path(0.1);
        outside.extend(path.iter());
        outside
    };
    scene.push_layer(Fill::EvenOdd, BlendMode::default(), 1.0, at, &outside);
    let faint = brush.clone().multiply_alpha(f32_of_f64(each));
    for ring in 0..rings {
        let pen = Stroke::new(grow(ring) * 2.0).with_join(Join::Round);
        scene.stroke(&pen, at, &faint, None, path);
    }
    scene.pop_layer();
}

/// A shadow: the mark's silhouette offset and, when soft, falling off over
/// `blur` units.
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
    if let Shape::Text { .. } = mark.shape {
        // Faint copies of the lettering around the offset: where they all
        // overlap, the shadow is as dark as asked.
        let rings = if shadow.blur > 0.0 { SHADOW_RINGS } else { 1 };
        let each = 1.0 - (1.0 - alpha.clamp(0.0, 0.999)).powf(1.0 / f64::from(rings));
        for ring in 0..rings {
            let turn = f64::from(ring) / f64::from(rings) * std::f64::consts::TAU;
            let jitter = Affine::translate((
                turn.cos() * shadow.blur * 0.5,
                turn.sin() * shadow.blur * 0.5,
            ));
            let faint = ink.with_alpha(f32_of_f64(each));
            draw_text_shape(&mark.shape, faint, scene, jitter * at, text);
        }
        return;
    }
    let filled = mark.shape.closed() || matches!(mark.shape, Shape::Stroke { .. });
    let line = if filled {
        0.0
    } else {
        mark.style.stroke_width.max(1.0)
    };
    soft_paint(
        scene,
        at,
        &ink.into(),
        &silhouette(mark),
        (filled, line),
        shadow.blur,
        alpha,
    );
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
        tracking,
        ..
    } = shape
    else {
        return None;
    };
    let family = match font.as_deref().and_then(crate::fonts::family) {
        None => ui::FontChoice::SansSerif,
        Some(family) => ui::FontChoice::Named(family.to_owned()),
    };
    let weight = weight.unwrap_or(400);
    let tier = match weight {
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
        ui::TextStyle::content(f32_of_f64(*size))
            .with_family(family)
            .with_weight(tier)
            // The shipped faces are variable: any weight between their
            // lightest and boldest draws as given.
            .with_font_variation(ui::OpenTypeTag::WEIGHT, f32::from(weight))
            .with_italic(*italic)
            .with_tracking(f32_of_f64(tracking.unwrap_or(0.0) / 1000.0))
            .with_line_height(f32_of_f64(line_height.unwrap_or(1.2)))
            .with_align(align),
    )
}

/// Where a text's lines start: its left edge, the wrap width it is laid
/// out in, and how far its first baseline sits below the layout's top.
fn text_frame(
    shape: &Shape,
    style: &ui::TextStyle,
    text: &mut TextRenderer,
) -> Option<(f64, f64, Option<f32>)> {
    let Shape::Text {
        x,
        y,
        text: words,
        align,
        width,
        ..
    } = shape
    else {
        return None;
    };
    let natural = f64::from(text.measure_style(words, width.map(f32_of_f64), style).0);
    let wide = width.unwrap_or(natural + 0.5);
    // Letter spacing follows every letter, the last too: a centred or
    // end-aligned line would sit that trailing space off where its letters
    // should be.
    let trail = f64::from(style.letter_spacing);
    let left = match (width, align.as_deref()) {
        (None, Some("center")) => x - wide / 2.0 + trail / 2.0,
        (None, Some("end")) => x - wide + trail,
        (Some(_), Some("center")) => x + trail / 2.0,
        (Some(_), Some("end")) => x + trail,
        _ => *x,
    };
    let max = (width.is_some() || align.as_deref().is_some_and(|align| align != "start"))
        .then(|| f32_of_f64(wide));
    let top = y - f64::from(text.first_baseline_style(words, max, style));
    Some((left, top, max))
}

/// One letter of a text as geometry in artwork units (before the mark's
/// transform), and the characters it shows.
pub struct Letter {
    pub path: BezPath,
    pub text: String,
}

/// A text shape's letters as outlines, where they are drawn: along its path
/// when it has one, else laid out from x, y.
pub fn letters(shape: &Shape, text: &mut TextRenderer) -> Vec<Letter> {
    let (
        Some(style),
        Shape::Text {
            text: words,
            path,
            align,
            ..
        },
    ) = (text_style(shape), shape)
    else {
        return Vec::new();
    };
    let named = |range: &std::ops::Range<usize>, words: &str| {
        words.get(range.clone()).unwrap_or_default().to_owned()
    };
    if let Some(d) = path {
        let line = words.replace('\n', " ");
        let glyphs = text.glyph_outlines(&line, None, &style);
        let along = BezPath::from_svg(d).unwrap_or_default();
        return on_path(&glyphs, &along, align.as_deref())
            .into_iter()
            .map(|(path, glyph)| Letter {
                path,
                text: named(&glyph.cluster, &line),
            })
            .collect();
    }
    let Some((left, top, max)) = text_frame(shape, &style, text) else {
        return Vec::new();
    };
    text.glyph_outlines(words, max, &style)
        .into_iter()
        .map(|glyph| Letter {
            path: Affine::translate((left + glyph.origin.x, top + glyph.origin.y))
                * glyph.path.clone(),
            text: named(&glyph.cluster, words),
        })
        .collect()
}

/// `glyphs` of one line set along `path`: each turned to the curve's
/// direction at its middle, the line placed at the path's start, middle or
/// end by `align`. Letters past either end are left out.
fn on_path<'a>(
    glyphs: &'a [ui::GlyphOutline],
    path: &BezPath,
    align: Option<&str>,
) -> Vec<(BezPath, &'a ui::GlyphOutline)> {
    let mut points: Vec<Point> = Vec::new();
    ui::vello::kurbo::flatten(path.iter(), 0.1, |element| match element {
        PathEl::MoveTo(at) if points.is_empty() => points.push(at),
        PathEl::MoveTo(at) | PathEl::LineTo(at) => points.push(at),
        _ => {}
    });
    let mut lengths = vec![0.0];
    for pair in points.windows(2) {
        if let [a, b] = pair {
            let last = lengths.last().copied().unwrap_or(0.0);
            lengths.push(last + a.distance(*b));
        }
    }
    let total = lengths.last().copied().unwrap_or(0.0);
    let line = glyphs
        .iter()
        .map(|glyph| glyph.origin.x + glyph.advance)
        .fold(0.0, f64::max);
    let start = match align {
        Some("center") => (total - line) / 2.0,
        Some("end") => total - line,
        _ => 0.0,
    };
    let baseline = glyphs.first().map_or(0.0, |glyph| glyph.origin.y);
    let at = |distance: f64| -> Option<(Point, f64)> {
        if !(0.0..=total).contains(&distance) {
            return None;
        }
        let index = lengths.partition_point(|length| *length < distance).max(1);
        let (a, b) = (*points.get(index - 1)?, *points.get(index)?);
        let span = (lengths.get(index)? - lengths.get(index - 1)?).max(1e-9);
        let t = (distance - lengths.get(index - 1)?) / span;
        Some((a.lerp(b, t), (b - a).atan2()))
    };
    glyphs
        .iter()
        .filter_map(|glyph| {
            let middle = start + glyph.origin.x + glyph.advance / 2.0;
            let (point, angle) = at(middle)?;
            let place = Affine::translate(point.to_vec2())
                * Affine::rotate(angle)
                * Affine::translate((-glyph.advance / 2.0, glyph.origin.y - baseline));
            Some((place * glyph.path.clone(), glyph))
        })
        .collect()
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
            text: words, path, ..
        },
    ) = (text_style(shape), shape)
    else {
        return;
    };
    if path.is_some() {
        for letter in letters(shape, text) {
            scene.fill(Fill::NonZero, at, ink, None, &letter.path);
        }
        return;
    }
    let Some((left, top, max)) = text_frame(shape, &style, text) else {
        return;
    };
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
    draw_vector_path(mark, palette, scene, transform, text, None);
}

fn draw_vector_path(
    mark: &Mark,
    palette: &[Pigment],
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    cached: Option<&BezPath>,
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
        Shape::Stroke { .. } => {
            let preset = crate::brushes::Preset::of(style);
            if preset.erases() {
                return;
            }
            let Some(ink) = named(&style.stroke).or_else(|| named(&style.fill)) else {
                return;
            };
            let path = silhouette(mark);
            let ink = ink.multiply_alpha(f32_of_f64(crate::brushes::ink_alpha(style)));
            scene.fill(Fill::NonZero, at, ink, None, &path);
        }
        Shape::Text { .. } => {
            let Some(ink) = named(&style.fill).or_else(|| named(&style.stroke)) else {
                return;
            };
            draw_text_shape(&mark.shape, ink, scene, at, text);
        }
        shape => {
            let fresh;
            let path = if let Some(path) = cached {
                path
            } else {
                fresh = shape.path();
                &fresh
            };
            if shape.closed() {
                if let Some(gradient) = style
                    .gradient
                    .as_ref()
                    .and_then(|gradient| gradient_brush(gradient, palette, opacity))
                {
                    scene.fill(Fill::NonZero, at, &gradient, None, path);
                } else if let Some(fill) = named(&style.fill) {
                    scene.fill(Fill::NonZero, at, fill, None, path);
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
                scene.stroke(&pen(style, width), at, line, None, path);
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
    let font = f32_of_f64((pixel * 11.0).max(0.1));
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
        text.draw_style(
            scene,
            &words,
            label,
            transform * Affine::translate((x + pixel * 2.0, y + pixel)),
            None,
            &ui::TextStyle::content(font),
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
