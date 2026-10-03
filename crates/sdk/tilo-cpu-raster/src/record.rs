//! Records the display list into a `vello_cpu` context for one region.
//!
//! Draws whose device bounds miss the region are not recorded, and a layer
//! whose clip misses it is skipped with everything inside, so the layer
//! stack stays balanced. A plain clip layer that cannot affect the region
//! (its clip is a rectangle containing the region) is not pushed, and other
//! plain clip layers become non-isolated clip paths.

use vello_cpu::RenderContext;
use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::{Affine, BezPath, Rect};
use vello_cpu::peniko::{Compose, Mix};

use crate::Stats;
use crate::blur::{self, BlurCache, BlurDraw};
use crate::decode::{Brush, DisplayList, LayerMode, Op, Style};

/// Caches and buffers reused across regions and frames.
#[derive(Default)]
pub(crate) struct Scratch {
    pub blurs: BlurCache,
    /// Whether blurred rects come from cached masks; see `Options::cached_blur`.
    pub cached_blur: bool,
    path: BezPath,
    /// Per open layer: how it was pushed to `vello_cpu`.
    pushed: Vec<Pushed>,
}

impl Scratch {
    pub fn new(cached_blur: bool) -> Self {
        Self {
            cached_blur,
            ..Self::default()
        }
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0
}

fn contains(outer: Rect, inner: Rect) -> bool {
    outer.x0 <= inner.x0 && outer.y0 <= inner.y0 && outer.x1 >= inner.x1 && outer.y1 >= inner.y1
}

/// The paint transform that, under geometry `transform`, puts the brush at
/// device `brush`. `None` when the geometry is degenerate.
fn relative(transform: Affine, brush: Affine) -> Option<Affine> {
    if transform == brush {
        return Some(Affine::IDENTITY);
    }
    let det = transform.determinant();
    (det.abs() > 1e-12).then(|| transform.inverse() * brush)
}

/// Set `brush` as the paint for geometry drawn under `transform`. Returns
/// false when nothing would be visible.
fn set_brush(ctx: &mut RenderContext, brush: &Brush, transform: Affine) -> bool {
    match brush {
        Brush::Solid(color) => {
            ctx.set_paint(*color);
            ctx.reset_paint_transform();
        }
        Brush::Gradient(gradient, brush_transform) => {
            let Some(paint) = relative(transform, *brush_transform) else {
                return false;
            };
            ctx.set_paint(gradient.clone());
            ctx.set_paint_transform(paint);
        }
        Brush::Image(image, brush_transform) => {
            let Some(paint) = relative(transform, *brush_transform) else {
                return false;
            };
            ctx.set_paint(image.clone());
            ctx.set_paint_transform(paint);
        }
    }
    true
}

fn paint_path(ctx: &mut RenderContext, path: &BezPath, style: &Style) {
    match style {
        Style::Fill(fill) => {
            ctx.set_fill_rule(*fill);
            ctx.fill_path(path);
        }
        Style::Stroke(stroke) => {
            ctx.set_stroke(stroke.clone());
            ctx.stroke_path(path);
        }
    }
}

/// How a layer is handed to `vello_cpu`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pushed {
    /// Not at all: its clip contains the region and it does nothing else.
    Nothing,
    /// As a clip path: a plain clip without a non-normal blend inside, where
    /// isolation cannot be observed and the cheaper non-isolated clip gives
    /// the same pixels.
    Clip,
    /// As an isolated layer.
    Layer,
}

fn how_to_push(
    list: &DisplayList,
    path: usize,
    transform: Affine,
    mode: LayerMode,
    nested_blend: bool,
    region: Rect,
) -> Pushed {
    let plain = match mode {
        LayerMode::Clip => true,
        LayerMode::Blend(blend, alpha) => {
            alpha >= 1.0 && blend.mix == Mix::Normal && blend.compose == Compose::SrcOver
        }
        LayerMode::LuminanceMask => false,
    };
    if !plain || nested_blend {
        return Pushed::Layer;
    }
    match blur::as_device_rect(list.path(path), transform) {
        Some(rect) if contains(rect, region) => Pushed::Nothing,
        _ => Pushed::Clip,
    }
}

fn pop(ctx: &mut RenderContext, pushed: Pushed) {
    match pushed {
        Pushed::Nothing => {}
        Pushed::Clip => ctx.pop_clip(),
        Pushed::Layer => ctx.pop_layer(),
    }
}

/// Record every draw touching `region` (device pixels). Returns how many
/// draws were recorded.
pub(crate) fn record(
    ctx: &mut RenderContext,
    list: &DisplayList,
    region: Rect,
    base: AlphaColor<Srgb>,
    scratch: &mut Scratch,
    stats: &mut Stats,
) -> usize {
    let offset = Affine::translate((-region.x0, -region.y0));
    let mut recorded = 0;
    scratch.pushed.clear();
    if base.components[3] > 0.0 {
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(base);
        ctx.fill_rect(&Rect::new(0.0, 0.0, region.width(), region.height()));
    }
    let mut i = 0;
    while let Some(op) = list.ops.get(i) {
        i += 1;
        match op {
            Op::Draw {
                path,
                transform,
                style,
                brush,
                bbox,
            } => {
                if !overlaps(*bbox, region) {
                    continue;
                }
                ctx.set_transform(offset * *transform);
                if set_brush(ctx, brush, *transform) {
                    paint_path(ctx, list.path(*path), style);
                    recorded += 1;
                }
            }
            Op::Glyphs {
                glyphs,
                style,
                brush,
                bbox,
            } => {
                if !overlaps(*bbox, region) {
                    continue;
                }
                let instances = list.glyphs.get(glyphs.clone()).unwrap_or_default();
                match style {
                    Style::Fill(fill) => {
                        // One path for the run, as vello draws it: glyphs
                        // that overlap are covered once.
                        let path = &mut scratch.path;
                        path.truncate(0);
                        for glyph in instances.iter().filter(|g| overlaps(g.bbox, region)) {
                            for el in glyph.outline.path.elements() {
                                path.push(glyph.transform * *el);
                            }
                        }
                        ctx.set_transform(offset);
                        if set_brush(ctx, brush, Affine::IDENTITY) {
                            ctx.set_fill_rule(*fill);
                            ctx.fill_path(path);
                        }
                    }
                    Style::Stroke(_) => {
                        for glyph in instances.iter().filter(|g| overlaps(g.bbox, region)) {
                            ctx.set_transform(offset * glyph.transform);
                            if set_brush(ctx, brush, glyph.transform) {
                                paint_path(ctx, &glyph.outline.path, style);
                            }
                        }
                    }
                }
                recorded += 1;
            }
            Op::Blur {
                path,
                transform,
                brush_transform,
                params,
                color,
                bbox,
            } => {
                if !overlaps(*bbox, region) {
                    continue;
                }
                let draw = BlurDraw {
                    shape: list.path(*path),
                    transform: *transform,
                    brush_transform: *brush_transform,
                    params: *params,
                    color: *color,
                    bbox: *bbox,
                };
                if blur::draw(ctx, &mut scratch.blurs, scratch.cached_blur, offset, &draw) {
                    stats.blur_misses += 1;
                }
                recorded += 1;
            }
            Op::Begin {
                path,
                transform,
                fill,
                mode,
                bbox,
                end,
                nested_blend,
            } => {
                let visible = path.is_some()
                    && overlaps(*bbox, region)
                    && !matches!(mode, LayerMode::LuminanceMask);
                if !visible {
                    // Nothing inside can show: skip to after the matching end.
                    i = end.saturating_add(1);
                    continue;
                }
                let Some(path) = *path else { continue };
                let how = how_to_push(list, path, *transform, *mode, *nested_blend, region);
                scratch.pushed.push(how);
                if how == Pushed::Nothing {
                    continue;
                }
                ctx.set_transform(offset * *transform);
                ctx.set_fill_rule(*fill);
                if how == Pushed::Clip {
                    ctx.push_clip_path(list.path(path));
                } else {
                    let (blend, alpha) = match mode {
                        LayerMode::Blend(blend, alpha) => (Some(*blend), Some(*alpha)),
                        _ => (None, None),
                    };
                    ctx.push_layer(Some(list.path(path)), blend, alpha, None, None);
                }
                recorded += 1;
            }
            Op::End => {
                if let Some(pushed) = scratch.pushed.pop() {
                    pop(ctx, pushed);
                }
            }
        }
    }
    while let Some(pushed) = scratch.pushed.pop() {
        pop(ctx, pushed);
    }
    recorded
}
