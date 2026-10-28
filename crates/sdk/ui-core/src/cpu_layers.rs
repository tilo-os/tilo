//! A frame drawn as layers a presenter composites: the desktop's ground and
//! chrome, each window of a strip, and the overlays above them, each
//! rasterized into a target of its own and drawn where it is. A frame that
//! only moved a layer — a strip that panned — draws no pixels at all: the
//! host moves the layer it already has ([`LayerDraw::at`]).
//!
//! [`crate::scene_parts::partition`] splits a tracked scene into these
//! layers; this keeps one [`CpuCanvas`] per layer between frames, repaints a
//! layer only where its own content changed, and hands the host what to
//! draw. The host composites: the page's WebGL2 presenter, or any other host
//! that draws a texture per layer at its place.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use crate::cpu_canvas::{CpuCanvas, Region};
use crate::retained_raster::FrameContent;
use crate::scene_parts::{DamageHint, SectionLayer, SectionParts};

/// One layer to draw this frame: the area of the layer's own target, where
/// its top left corner goes, and how opaque it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerDraw {
    /// The layer this is, as [`SectionLayer::key`].
    pub key: u64,
    /// The extent of the layer's target, which holds the whole of it.
    pub extent: (u32, u32),
    /// The pixels of it this frame shows: a layer a strip panned past the
    /// edge of the display keeps the pixels beyond it, and shows the rest.
    pub source: Region,
    /// Where the source's top left pixel goes, in device pixels.
    pub at: (i32, i32),
    /// What it composites with (1.0 unless a fade drew the layer).
    pub alpha: f32,
}

/// The most layers kept between frames; the least recently drawn go first.
const MAX_LAYERS: usize = 32;

/// One layer's own target and the content it holds.
struct Retained {
    key: u64,
    canvas: CpuCanvas,
    /// The layer's commands as its canvas holds them, in device pixels.
    scene: Scene,
    /// The damage parts inside them, in device pixels.
    parts: Option<SectionParts>,
    /// Device position of the canvas' top left corner.
    at: (i32, i32),
    /// Device position of the layer's own bounds origin when it was drawn:
    /// where its content sits, before any move since.
    origin: (f64, f64),
    /// Where the target's top left corner is drawn now: what the moves of
    /// the drawing since it was rasterized add up to.
    placed: (i32, i32),
    /// The hint the drawing gave in the frame it was last drawn in
    /// ([`SectionLayer::hint`]): the next frame's hint continues from it.
    hint: Option<DamageHint>,
    /// Where the frame before drew it, in device pixels.
    shown: (i32, i32),
    /// The frame this layer was last drawn in.
    used: u64,
}

/// Every layer of the frames a host draws, each kept between them.
pub struct CpuLayers {
    layers: Vec<Retained>,
    draws: Vec<LayerDraw>,
    /// What each draw of this frame did, for the host's frame metrics: `M`
    /// the layer moved and its pixels moved with it, `S` its commands only
    /// shifted, `D` it repainted its damage, `W` it was drawn whole for the
    /// first time, `C` it moved and changed, `I` its target holds nothing,
    /// `X` it moved somewhere its pixels did not reach, and `U` with the
    /// reason its damage could not be proven (`t` the parts differ, `e` the
    /// section's own draws changed, `b` a bound is missing, `p` no parts).
    kinds: Vec<String>,
    /// The pixels each layer rasterized this frame, in draw order.
    drawn: Vec<u64>,
    /// The frame being built, and the extent of the target: another extent
    /// drops what the layers hold.
    frame: u64,
    size: (u32, u32),
    /// Whether the layers' canvases still hold what they drew: a target made
    /// anew ([`Self::invalidate`]) leaves them stale.
    valid: bool,
    /// How the frame's own coordinates map onto the target's pixels.
    to_device: Affine,
    rasterized: u64,
    /// This frame moved layers under the host without drawing a pixel.
    moved: bool,
    /// The pixels noted for the layers [`Self::drawn`] has so far.
    noted: u64,
}

impl CpuLayers {
    /// A set of layers to draw frames into, or `None` where this host's
    /// rasterizer is not in yet ([`crate::cpu_canvas::install_cpu_raster`]).
    pub fn new() -> Option<Self> {
        CpuCanvas::new().map(|canvas| Self {
            layers: vec![Retained {
                key: 0,
                canvas,
                scene: Scene::new(),
                parts: None,
                at: (0, 0),
                origin: (0.0, 0.0),
                placed: (0, 0),
                hint: None,
                shown: (0, 0),
                used: 0,
            }],
            draws: Vec::new(),
            kinds: Vec::new(),
            drawn: Vec::new(),
            frame: 0,
            size: (0, 0),
            valid: false,
            to_device: Affine::IDENTITY,
            rasterized: 0,
            moved: false,
            noted: 0,
        })
    }

    /// What the layers hold is no longer what the host shows: every layer is
    /// drawn whole next frame.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// What the host draws this frame, in compositing order (the order the
    /// scene drew them in).
    pub fn draws(&self) -> &[LayerDraw] {
        &self.draws
    }

    /// What each of [`Self::draws`] did this frame.
    pub fn kinds(&self) -> &[String] {
        &self.kinds
    }

    /// What each layer did and how big its target is, for a host's frame
    /// metrics: `M520x300` is a layer that moved without drawing.
    pub fn report(&self) -> String {
        self.draws
            .iter()
            .zip(self.kinds.iter())
            .zip(self.drawn.iter())
            .map(|((draw, kind), drawn)| {
                format!(
                    "{kind}{}x{}@{},{}#{}",
                    draw.extent.0, draw.extent.1, draw.at.0, draw.at.1, drawn
                )
            })
            .collect::<Vec<_>>()
            .join("+")
    }

    /// The pixels rasterized for this frame, over every layer.
    pub fn rasterized(&self) -> u64 {
        self.rasterized
    }

    /// Whether this frame moved layers under the host and drew nothing.
    pub fn moved(&self) -> bool {
        self.moved
    }

    /// A layer's canvas — premultiplied RGBA8, rows of `width * 4` bytes —
    /// its extent, and the regions [`Self::update`] last drew.
    pub fn layer(&self, key: u64) -> Option<(&[u8], (u32, u32), &[Region])> {
        let layer = self.layers.iter().find(|layer| layer.key == key)?;
        Some((
            layer.canvas.pixels(),
            layer.canvas.extent(),
            layer.canvas.drawn(),
        ))
    }

    /// Bring every layer up to date with the frame `layers` describe, in
    /// draw order, and work out what the host draws for them.
    ///
    /// A layer whose commands only moved is drawn where it moved and
    /// rasterizes nothing; one whose content changed repaints its damage; a
    /// layer that moved and changed repaints whole, and one this host has no
    /// target for yet draws whole.
    ///
    /// # Errors
    ///
    /// When a layer cannot be rasterized at its extent.
    pub fn update(
        &mut self,
        layers: &[SectionLayer],
        size: (u32, u32),
        to_device: Affine,
    ) -> Result<u64, String> {
        if size != self.size {
            self.size = size;
            self.valid = false;
        }
        self.to_device = to_device;
        self.frame = self.frame.saturating_add(1);
        self.draws.clear();
        self.kinds.clear();
        self.drawn.clear();
        self.rasterized = 0;
        self.moved = false;
        for layer in layers {
            if let Some(draw) = self.layer_draw(layer, size)? {
                self.draws.push(draw);
            }
        }
        // What this frame did not draw is dropped: a window that left the
        // strip has no pixels worth keeping.
        let frame = self.frame;
        self.layers.retain(|layer| layer.used == frame);
        self.evict();
        self.valid = true;
        Ok(self.rasterized)
    }

    /// Keep at most [`MAX_LAYERS`] between frames.
    fn evict(&mut self) {
        while self.layers.len() > MAX_LAYERS {
            let oldest = self
                .layers
                .iter()
                .enumerate()
                .min_by_key(|(_, layer)| layer.used)
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.layers.remove(oldest);
        }
    }

    /// One layer of the frame: what the host draws for it, after drawing
    /// whatever of it changed.
    fn layer_draw(
        &mut self,
        layer: &SectionLayer,
        size: (u32, u32),
    ) -> Result<Option<LayerDraw>, String> {
        // What of the target the layer may paint, in device pixels, and
        // where the screen shows it: a strip drawn in canvas coordinates
        // draws its windows where they rest and the screen shows them a pan
        // away.
        let viewport = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
        let shown = translated(layer.bounds, layer.offset);
        let Some(visible) = region_of(shown.intersect(viewport)) else {
            return Ok(None);
        };
        // The target holds the layer's whole extent, so a strip drawn in
        // canvas coordinates pans over pixels it already drew, with a pixel
        // of slack around them for a move of a fraction of a pixel. A layer
        // far bigger than the display holds what the display can show.
        let here = translated(viewport, (-layer.offset.0, -layer.offset.1));
        let entire = region_of(layer.bounds.inflate(1.0, 1.0));
        let mut area = region_of(layer.bounds.intersect(here).inflate(1.0, 1.0))
            .ok_or_else(|| "a layer paints nothing the display shows".to_owned())?;
        if let Some(entire) = entire
            && u64::from(entire.width) * u64::from(entire.height)
                <= 2 * u64::from(size.0) * u64::from(size.1)
        {
            area = entire;
        }
        let Some(entry) = self.layers.iter().position(|held| held.key == layer.key) else {
            return self.draw_new(layer, &area).map(Some);
        };
        self.layers[entry].used = self.frame;
        // Where the frame before showed it: a layer drawn somewhere else was
        // moved, and its pixels came with it.
        let at_now = (visible.x as i32, visible.y as i32);
        if self.layers[entry].shown != at_now {
            self.moved = true;
            self.layers[entry].shown = at_now;
        }
        let Some(held) = self.layers.get(entry) else {
            return Ok(None);
        };
        let (anchor, extent, placed, used) =
            (held.at, held.canvas.extent(), held.placed, held.used);
        let source = inside(&visible, layer.offset, anchor);
        // The layer's drawing says its pixels are the ones it drew before,
        // moved: the host draws those pixels where the layer is now, and
        // nothing is rasterized.
        if let Some(hint) = layer.hint.as_ref()
            && let Some(moved) = hint.moved_by()
            && moved != (0.0, 0.0)
            && (used.saturating_add(1) == self.frame
                || held
                    .hint
                    .as_ref()
                    .is_some_and(|previous| follows(previous, hint)))
        {
            let at = (
                placed.0.saturating_add(whole(moved.0)),
                placed.1.saturating_add(whole(moved.1)),
            );
            if let Some(cropped) = moved_source(&visible, layer.offset, extent, at) {
                self.moved = true;
                self.layers[entry].placed = at;
                self.layers[entry].hint = Some(hint.clone());
                self.note("M");
                return Ok(Some(LayerDraw {
                    key: layer.key,
                    extent,
                    source: cropped,
                    at: (visible.x as i32, visible.y as i32),
                    alpha: 1.0,
                }));
            }
            self.note("X");
            return self.draw_whole(entry, layer, &area).map(Some);
        }
        // The layer's commands, moved by how far the layer moved: the same
        // drawing at another place rasterizes nothing, the host moves the
        // pixels it has.
        let moved = (
            layer.bounds.x0 - held.origin.0,
            layer.bounds.y0 - held.origin.1,
        );
        let moved_to = (
            placed.0.saturating_add(whole(moved.0)),
            placed.1.saturating_add(whole(moved.1)),
        );
        if !self.valid {
            self.note("I");
            return self.draw_whole(entry, layer, &area).map(Some);
        }
        if moved_to != anchor && content_moved(&held.scene, &layer.scene, moved) {
            if let Some(cropped) = moved_source(&visible, layer.offset, extent, moved_to) {
                self.moved = true;
                self.note("S");
                return Ok(Some(LayerDraw {
                    key: layer.key,
                    extent,
                    source: cropped,
                    at: (visible.x as i32, visible.y as i32),
                    alpha: 1.0,
                }));
            }
            return self.draw_whole(entry, layer, &area).map(Some);
        }
        if moved_to != anchor {
            // It moved and what it draws changed under it.
            self.note("C");
            return self.draw_whole(entry, layer, &area).map(Some);
        }
        if anchor != (area.x as i32, area.y as i32) {
            // It shows a part of itself this target does not hold.
            self.note("C");
            return self.draw_whole(entry, layer, &area).map(Some);
        }
        // The same commands as the frame it drew: nothing in it changed, and
        // nothing is worth proving.
        let damage = if held.scene.encoding_eq(&layer.scene) {
            Ok(Vec::new())
        } else {
            match (&held.parts, &layer.parts) {
                (Some(previous), Some(current)) => {
                    crate::scene_parts::damage(&held.scene, previous, &layer.scene, current)
                        .map(|rects| to_canvas(&rects, anchor))
                        .map_err(why_not)
                }
                // No parts to compare inside it: it is drawn whole.
                _ => Err("p".to_owned()),
            }
        };
        let damage = match damage {
            Ok(damage) => damage,
            Err(reason) => {
                self.note(&format!("U{reason}"));
                return self.draw_whole(entry, layer, &area).map(Some);
            }
        };
        if !damage.is_empty() {
            self.rasterize(entry, layer, &damage)?;
        }
        self.note("D");
        Ok(Some(LayerDraw {
            key: layer.key,
            extent,
            source,
            at: (visible.x as i32, visible.y as i32),
            alpha: 1.0,
        }))
    }

    /// Draw a layer this host has no target for.
    fn note(&mut self, kind: &str) {
        self.kinds.push(kind.to_owned());
        self.drawn.push(self.rasterized.saturating_sub(self.noted));
        self.noted = self.rasterized;
    }

    fn draw_new(&mut self, layer: &SectionLayer, area: &Region) -> Result<LayerDraw, String> {
        let canvas = CpuCanvas::new()
            .ok_or_else(|| "the CPU rasterizer this host draws with is not in".to_owned())?;
        let mut held = Retained {
            key: layer.key,
            canvas,
            scene: Scene::new(),
            parts: None,
            at: (area.x as i32, area.y as i32),
            origin: (layer.bounds.x0, layer.bounds.y0),
            placed: (area.x as i32, area.y as i32),
            hint: None,
            shown: (0, 0),
            used: self.frame,
        };
        let draw = self.store_whole(&mut held, layer, area)?;
        self.layers.push(held);
        self.note("W");
        Ok(draw)
    }

    /// Draw layer `entry` whole, anchored where it is now.
    fn draw_whole(
        &mut self,
        entry: usize,
        layer: &SectionLayer,
        area: &Region,
    ) -> Result<LayerDraw, String> {
        let mut held = self.layers.remove(entry);
        held.at = (area.x as i32, area.y as i32);
        held.origin = (layer.bounds.x0, layer.bounds.y0);
        let draw = self.store_whole(&mut held, layer, area)?;
        self.layers.push(held);
        self.note("W");
        Ok(draw)
    }

    /// Rasterize all of `layer` into `held`, whose canvas is at its place.
    fn store_whole(
        &mut self,
        held: &mut Retained,
        layer: &SectionLayer,
        area: &Region,
    ) -> Result<LayerDraw, String> {
        let extent = (area.width, area.height);
        self.rasterized = self
            .rasterized
            .saturating_add(u64::from(extent.0).saturating_mul(u64::from(extent.1)));
        rasterize(held, layer, self.to_device, extent, &[], true)?;
        Ok(LayerDraw {
            key: layer.key,
            extent,
            source: Region::new(0, 0, extent.0, extent.1),
            at: held.at,
            alpha: 1.0,
        })
    }

    /// Repaint `damage` pixels of layer `entry`.
    fn rasterize(
        &mut self,
        entry: usize,
        layer: &SectionLayer,
        damage: &[Rect],
    ) -> Result<(), String> {
        let held = &mut self.layers[entry];
        let extent = held.canvas.extent();
        self.rasterized = self
            .rasterized
            .saturating_add(damage.iter().map(|rect| rect_area(rect)).sum::<u64>());
        rasterize(held, layer, self.to_device, extent, damage, false)
    }
}

/// Rasterize `layer` into `held`'s canvas at the pixels `source` covers
/// (canvas pixels), keeping what was rasterized: `damage` in canvas pixels,
/// or all of it when `whole`.
fn rasterize(
    held: &mut Retained,
    layer: &SectionLayer,
    to_device: Affine,
    extent: (u32, u32),
    damage: &[Rect],
    whole: bool,
) -> Result<(), String> {
    // The layer's own coordinates, in the target's pixels, from the canvas'
    // top left corner.
    let local_transform =
        to_device * Affine::translate((-f64::from(held.at.0), -f64::from(held.at.1)));
    let mut local = Scene::new();
    if local_transform == Affine::IDENTITY {
        local.append(&layer.scene, None);
    } else {
        local.append(&layer.scene, Some(local_transform));
    }
    let backdrop = crate::backdrop::BackdropFrame::default();
    held.canvas.rasterize(&FrameContent {
        scene: &local,
        width: extent.0,
        height: extent.1,
        base_color: Color::TRANSPARENT,
        damage_regions: damage,
        scroll_copies: &[],
        full_redraw: whole,
        backdrop: &backdrop,
    })?;
    // What the canvas now holds, at the place it was drawn.
    held.scene.clone_from(&layer.scene);
    held.parts.clone_from(&layer.parts);
    held.placed = held.at;
    held.hint = layer.hint.clone();
    Ok(())
}

/// Whether `to` is `from` drawn at another place: the same commands, through
/// transforms that all moved by `moved`.
fn content_moved(from: &Scene, to: &Scene, moved: (f64, f64)) -> bool {
    if moved == (0.0, 0.0) {
        return from.encoding_eq(to);
    }
    let (before, after) = (from.encoding(), to.encoding());
    if before.transforms.is_empty()
        || before.transforms.len() != after.transforms.len()
        || before.draw_tags.len() != after.draw_tags.len()
    {
        return false;
    }
    // vello composes a translate into a command's transform by adding to it,
    // exactly as this does, so a drawing replayed at another place compares
    // equal; a fresh draw of it may round differently and is rasterized.
    let mut candidate = Scene::new();
    candidate.append(from, Some(Affine::translate(moved)));
    candidate.encoding_eq(to)
}

/// How many pixels `rect` covers.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a damage rect is a bounded area of a rasterized target"
)]
fn rect_area(rect: &Rect) -> u64 {
    (rect.width().max(0.0) * rect.height().max(0.0)) as u64
}

/// Why a layer's damage could not be proven, in one letter.
fn why_not(unproven: crate::scene_parts::Unproven) -> String {
    use crate::scene_parts::Unproven;
    match unproven {
        Unproven::Structure => "t",
        Unproven::Section => "e",
        Unproven::Unbounded => "b",
        Unproven::Corrupt => "c",
    }
    .to_owned()
}

/// Whether `current`'s move counts from `previous`'s content: the same
/// source, placed the same way, one generation later.
fn follows(previous: &DamageHint, current: &DamageHint) -> bool {
    previous.to == current.from
        && previous.source == current.source
        && previous.placement == current.placement
}

/// `delta` in whole device pixels: a layer that moves by a fraction of one is
/// drawn at the pixel it is nearest.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a layer's move is bounded by the display, far inside i32's range"
)]
fn whole(delta: f64) -> i32 {
    if !delta.is_finite() {
        return 0;
    }
    delta.round() as i32
}

/// The pixels of a canvas whose top left corner sits at `placed` that show
/// `visible` (device pixels) of a layer whose own coordinates are `offset`
/// away, or `None` when the layer moved somewhere the pixels it kept do not
/// reach (it grew into the display, say).
fn moved_source(
    visible: &Region,
    offset: (f64, f64),
    extent: (u32, u32),
    placed: (i32, i32),
) -> Option<Region> {
    let x = f64::from(visible.x) - offset.0 - f64::from(placed.0);
    let y = f64::from(visible.y) - offset.1 - f64::from(placed.1);
    // The canvas keeps a pixel of slack around the layer's own bounds, which
    // a move of a fraction of a pixel needs.
    if x < -1.0 || y < -1.0 {
        return None;
    }
    if x + f64::from(visible.width) > f64::from(extent.0) + 1.0
        || y + f64::from(visible.height) > f64::from(extent.1) + 1.0
    {
        return None;
    }
    Some(inside(visible, offset, placed))
}

/// `rect` moved by `offset`.
fn translated(rect: Rect, (x, y): (f64, f64)) -> Rect {
    Rect::new(rect.x0 + x, rect.y0 + y, rect.x1 + x, rect.y1 + y)
}

/// `visible`, in device pixels, as pixels of a canvas anchored at `anchor`
/// in the layer's own coordinates.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a layer's coordinates are bounded by the display"
)]
fn inside(visible: &Region, offset: (f64, f64), anchor: (i32, i32)) -> Region {
    let x = f64::from(visible.x) - offset.0 - f64::from(anchor.0);
    let y = f64::from(visible.y) - offset.1 - f64::from(anchor.1);
    Region::new(
        x.max(0.0).round() as u32,
        y.max(0.0).round() as u32,
        visible.width,
        visible.height,
    )
}

/// `rects`, in device coordinates, as canvas pixels around `at`.
fn to_canvas(rects: &[Rect], at: (i32, i32)) -> Vec<Rect> {
    let (x, y) = (f64::from(at.0), f64::from(at.1));
    rects
        .iter()
        .map(|rect| Rect::new(rect.x0 - x, rect.y0 - y, rect.x1 - x, rect.y1 - y))
        .collect()
}

/// The device pixels `bounds` covers, rounded outward, or `None` when it
/// covers none.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "device rects are clamped to the target's extent before narrowing"
)]
fn region_of(bounds: Rect) -> Option<Region> {
    if !bounds.x0.is_finite() || !bounds.y0.is_finite() {
        return None;
    }
    let x0 = bounds.x0.floor().max(0.0);
    let y0 = bounds.y0.floor().max(0.0);
    let x1 = bounds.x1.ceil().max(0.0);
    let y1 = bounds.y1.ceil().max(0.0);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Region::new(
        x0 as u32,
        y0 as u32,
        (x1 - x0) as u32,
        (y1 - y0) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scene_parts::{self, PartKey, Recording};
    use vello::peniko::Fill;

    const SIZE: (u32, u32) = (200, 100);

    fn fill(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        scene.fill(Fill::NonZero, transform, color, None, &rect);
    }

    /// A window's own drawing: a plate with a border and a caret inside it,
    /// recorded once and drawn where the strip puts it.
    fn window() -> Scene {
        let mut scene = Scene::new();
        fill(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 60.0, 40.0),
            Color::from_rgba8(40, 80, 160, 255),
        );
        fill(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(4.0, 4.0, 20.0, 20.0),
            Color::from_rgba8(220, 220, 220, 255),
        );
        scene
    }

    /// A frame: the desktop's own draws, a window layer at `at`, more of the
    /// desktop's draws above it.
    fn frame(at: f64, window: &Scene, shade: u8) -> (Box<Scene>, scene_parts::SectionParts) {
        let recording = Recording::begin();
        let mut scene = Box::new(Scene::new());
        scene_parts::track(&scene);
        fill(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 100.0),
            Color::from_rgba8(20, 20, 24, 255),
        );
        let placed = Affine::translate((at, 20.0));
        scene_parts::part(
            &mut scene,
            PartKey::layer("test.window", 1_u64),
            vec![Rect::new(at, 20.0, at + 60.0, 60.0)],
            |scene| {
                // The window's content is a part of its own, as a window's
                // is: a change inside it damages its bounds, not the layer's.
                scene_parts::part(
                    scene,
                    PartKey::new("test.content", 1_u64),
                    vec![Rect::new(at, 20.0, at + 60.0, 60.0)],
                    |scene| {
                        // The drawing replayed where the strip puts it, as a
                        // strip that only panned does.
                        scene.append(window, Some(placed));
                    },
                );
                scene_parts::part(
                    scene,
                    PartKey::new("test.caret", 1_u64),
                    vec![Rect::new(at + 10.0, 30.0, at + 12.0, 38.0)],
                    |scene| {
                        fill(
                            scene,
                            placed,
                            Rect::new(10.0, 10.0, 12.0, 18.0),
                            Color::from_rgba8(shade, shade, shade, 255),
                        );
                    },
                );
            },
        );
        // The overlay above the window, drawn by the desktop itself.
        fill(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(150.0, 0.0, 200.0, 8.0),
            Color::from_rgba8(200, 120, 40, 255),
        );
        let parts = recording
            .finish()
            .of(&scene)
            .expect("a tracked, well-nested section has parts");
        (scene, parts)
    }

    fn layers(scene: &Scene, parts: &scene_parts::SectionParts) -> Vec<SectionLayer> {
        let mut bounds = crate::scene_parts::DrawBounds::default();
        scene_parts::partition(scene, parts, Affine::IDENTITY, &mut bounds)
            .expect("the frame is drawn as layers")
    }

    /// What every layer composited in order makes of the target: the host's
    /// own compositing, on the CPU.
    fn composite(store: &CpuLayers) -> Vec<u8> {
        let mut target = vec![0u8; (SIZE.0 * SIZE.1 * 4) as usize];
        for draw in store.draws() {
            let (pixels, extent, _) = store.layer(draw.key).expect("a drawn layer's pixels");
            for y in 0..draw.source.height {
                for x in 0..draw.source.width {
                    let (px, py) = (draw.source.x + x, draw.source.y + y);
                    let source = ((py * extent.0 + px) * 4) as usize;
                    let (dx, dy) = (draw.at.0 + x as i32, draw.at.1 + y as i32);
                    if dx < 0 || dy < 0 || dx as u32 >= SIZE.0 || dy as u32 >= SIZE.1 {
                        continue;
                    }
                    let at = ((dy as u32 * SIZE.0 + dx as u32) * 4) as usize;
                    let alpha = u32::from(pixels[source + 3]);
                    for channel in 0..4 {
                        let under = u32::from(target[at + channel]);
                        let over = u32::from(pixels[source + channel]);
                        let blended = over + (under * (255 - alpha) + 127) / 255;
                        target[at + channel] = blended.min(255) as u8;
                    }
                }
            }
        }
        target
    }

    /// The same frame drawn whole into one target.
    fn whole(scene: &Scene) -> Vec<u8> {
        let mut canvas = CpuCanvas::new().expect("a canvas to draw on");
        let backdrop = crate::backdrop::BackdropFrame::default();
        canvas
            .rasterize(&FrameContent {
                scene,
                width: SIZE.0,
                height: SIZE.1,
                base_color: Color::from_rgba8(20, 20, 24, 255),
                damage_regions: &[],
                scroll_copies: &[],
                full_redraw: true,
                backdrop: &backdrop,
            })
            .expect("a whole frame");
        canvas.pixels().to_vec()
    }

    fn first(store: &mut CpuLayers, scene: &Scene, parts: &scene_parts::SectionParts) {
        let layers = layers(scene, parts);
        store
            .update(&layers, SIZE, Affine::IDENTITY)
            .expect("the frame draws");
    }

    #[test]
    fn a_frame_is_the_layers_composited_in_draw_order() {
        let window = window();
        let (scene, parts) = frame(40.0, &window, 220);
        let mut store = CpuLayers::new().expect("a canvas to draw with");
        first(&mut store, &scene, &parts);
        let drawn = store.draws().to_vec();
        assert_eq!(drawn.len(), 3, "ground, window, overlay: {drawn:?}");
        assert_eq!(
            drawn.iter().map(|draw| draw.at).collect::<Vec<_>>(),
            vec![(0, 0), (39, 19), (149, 0)],
            "each layer shows where it was drawn"
        );
        assert_eq!(
            drawn.iter().map(|draw| draw.extent).collect::<Vec<_>>(),
            vec![(201, 101), (62, 42), (52, 9)],
            "each layer's target is its own extent and a pixel of slack"
        );
        assert_eq!(
            composite(&store),
            whole(&scene),
            "the layers composited are the frame"
        );
    }

    #[test]
    fn a_frame_that_only_moved_a_layer_rasterizes_nothing() {
        let window = window();
        let (first_frame, first_parts) = frame(40.0, &window, 220);
        let mut store = CpuLayers::new().expect("a canvas to draw with");
        first(&mut store, &first_frame, &first_parts);
        assert_eq!(composite(&store), whole(&first_frame));

        let (panned, parts) = frame(56.0, &window, 220);
        let layers = layers(&panned, &parts);
        let rasterized = store
            .update(&layers, SIZE, Affine::IDENTITY)
            .expect("the panned frame draws");
        assert_eq!(rasterized, 0, "a strip that panned draws no pixel");
        assert!(store.moved(), "the window moved");
        let window_draw = store
            .draws()
            .iter()
            .find(|draw| draw.extent == (62, 42))
            .expect("the window is drawn at its new place");
        assert_eq!(
            window_draw.at,
            (56, 20),
            "the window's pixels moved with it"
        );
        assert_eq!(
            window_draw.source,
            Region::new(1, 1, 60, 40),
            "and the whole of what it drew shows"
        );
        let composed = composite(&store);
        let rendered = whole(&panned);
        let differing: Vec<(usize, u8, u8)> = composed
            .iter()
            .zip(&rendered)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(at, (a, b))| (at, *a, *b))
            .take(8)
            .collect();
        assert!(
            composed == rendered,
            "the pan shows the pixels it moved: {} bytes differ, {differing:?}",
            composed
                .iter()
                .zip(&rendered)
                .filter(|(a, b)| a != b)
                .count()
        );
    }

    #[test]
    fn a_layer_that_changed_repaints_only_its_damage() {
        let window = window();
        let (scene, parts) = frame(40.0, &window, 220);
        let mut store = CpuLayers::new().expect("a canvas to draw with");
        first(&mut store, &scene, &parts);
        // The caret inside the window changes: the window repaints the
        // caret's pixels and no other layer repaints anything.
        let (typed, parts) = frame(40.0, &window, 255);
        let layers = layers(&typed, &parts);
        let rasterized = store
            .update(&layers, SIZE, Affine::IDENTITY)
            .expect("the frame draws");
        assert!(
            rasterized > 0 && rasterized < 60 * 40 / 4,
            "typing repainted {rasterized} pixels, more than the caret's reach"
        );
        assert_eq!(composite(&store), whole(&typed));
    }

    #[test]
    fn a_change_in_one_layer_rasterizes_no_other() {
        let window = window();
        let (scene, parts) = frame(40.0, &window, 220);
        let mut store = CpuLayers::new().expect("a canvas to draw with");
        first(&mut store, &scene, &parts);
        // The window's content scrolls: its own layer repaints what the
        // scroll exposed, and the ground and the overlay repaint nothing.
        let (scrolled, parts) = frame(40.0, &window, 255);
        let framed = layers(&scrolled, &parts);
        store
            .update(&framed, SIZE, Affine::IDENTITY)
            .expect("the frame draws");
        let drawn: Vec<(&str, u32)> = store
            .draws()
            .iter()
            .zip(store.kinds())
            .zip(store.drawn.iter())
            .map(|((draw, kind), _)| (kind.as_str(), draw.extent.0))
            .collect();
        assert_eq!(drawn.len(), 3, "ground, window, overlay: {drawn:?}");
        assert_eq!(drawn[0].0, "D", "the ground repaints nothing: {drawn:?}");
        assert_eq!(drawn[2].0, "D", "the overlay repaints nothing: {drawn:?}");
        assert!(
            store
                .drawn
                .iter()
                .take(1)
                .chain(store.drawn.iter().skip(2))
                .all(|pixels| *pixels == 0),
            "only the window's layer repainted: {:?}",
            store.drawn
        );
        assert_eq!(composite(&store), whole(&scrolled));
    }

    #[test]
    fn a_layer_drawn_inside_an_alpha_layer_is_not_one() {
        let window = window();
        let recording = Recording::begin();
        let mut scene = Box::new(Scene::new());
        scene_parts::track(&scene);
        fill(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 100.0),
            Color::from_rgba8(20, 20, 24, 255),
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            0.5,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 200.0, 100.0),
        );
        scene_parts::part(
            &mut scene,
            PartKey::layer("test.window", 1_u64),
            vec![Rect::new(40.0, 20.0, 100.0, 60.0)],
            |scene| scene.append(&window, Some(Affine::translate((40.0, 20.0)))),
        );
        scene.pop_layer();
        let parts = recording.finish().of(&scene).expect("parts");
        let layers = layers(&scene, &parts);
        assert_eq!(
            layers.len(),
            1,
            "the window is drawn with the ground it fades over, not as a layer of its own"
        );
        assert!(layers[0].run);
    }
}
