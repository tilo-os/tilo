//! Blurred rounded rectangles from cached coverage masks.
//!
//! vello evaluates its blurred-rounded-rect approximation (three `pow`s and
//! two `erf`s) for every covered pixel, interior included. Here the same
//! formula, evaluated where vello's fine shader evaluates it, fills a coverage
//! mask once per distinct blur; each frame then draws the mask as a tinted
//! image over the edge band and a solid rectangle over the interior, where
//! the blur is constant. Masks are keyed by the blur's size in device pixels
//! and its sub-pixel phase (in eighths), so moving a shadow reuses its mask.

use std::collections::HashMap;
use std::sync::Arc;

use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::{Affine, BezPath, PathEl, Point, Rect};
use vello_cpu::peniko::{Extend, Fill, ImageQuality, ImageSampler};
use vello_cpu::{Image, ImageSource, PixelMetadata, Pixmap, RenderContext, Tint, TintMode};

use crate::decode::BlurParams;

/// Sub-pixel phase steps per pixel a mask is computed for.
const PHASES: f64 = 8.0;
/// Frames a mask may go unused before it is dropped.
const MAX_IDLE_FRAMES: u64 = 60;
/// Masks larger than this many pixels are evaluated per frame by `vello_cpu`.
const MAX_MASK_PIXELS: usize = 4096 * 4096;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    params: [u32; 4],
    scale: [u32; 2],
    phase: [i8; 2],
    /// Mask extents in pixels relative to the blur's integer origin.
    extents: [i32; 4],
}

struct Mask {
    pixmap: Arc<Pixmap>,
    /// Interior in mask pixels where coverage is the constant `interior_alpha`.
    interior: Option<(i32, i32, i32, i32)>,
    interior_alpha: f32,
    last_used: u64,
}

#[derive(Default)]
pub(crate) struct BlurCache {
    masks: HashMap<Key, Mask>,
    frame: u64,
    scratch: BezPath,
}

impl BlurCache {
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.masks
            .retain(|_, mask| frame - mask.last_used <= MAX_IDLE_FRAMES);
    }
}

/// The derived constants of vello's blurred rounded rectangle.
struct Shape {
    width: f32,
    height: f32,
    r1: f32,
    exponent: f32,
    inv_exponent: f32,
    scale: f32,
    inv_std_dev: f32,
    min_edge: f32,
    std_dev: f32,
}

impl Shape {
    fn new(p: BlurParams) -> Self {
        let std_dev = p.std_dev.max(1e-5);
        let inv_std_dev = 1.0 / std_dev;
        let min_edge = p.width.min(p.height);
        let radius_max = 0.5 * min_edge;
        let r0 = p.radius.hypot(std_dev * 1.15).min(radius_max);
        let r1 = p.radius.hypot(std_dev * 2.0).min(radius_max);
        let exponent = 2.0 * r1 / r0;
        let delta = 1.25
            * std_dev
            * ((-(0.5 * inv_std_dev * p.width).powi(2)).exp()
                - (-(0.5 * inv_std_dev * p.height).powi(2)).exp());
        let width = p.width + delta.min(0.0);
        let height = p.height - delta.max(0.0);
        let scale = 0.5 * erf7(inv_std_dev * 0.5 * (width.max(height) - 0.5 * p.radius));
        Self {
            width,
            height,
            r1,
            exponent,
            inv_exponent: 1.0 / exponent,
            scale,
            inv_std_dev,
            min_edge,
            std_dev,
        }
    }

    /// Distance term along one axis: positive in the corner zone.
    fn axis(&self, v: f32, extent: f32) -> f32 {
        v.abs() - (extent * 0.5 - self.r1)
    }

    fn alpha(&self, x0: f32, xe: f32, y0: f32, ye: f32) -> f32 {
        if x0 > 0.0 && y0 > 0.0 {
            self.profile((xe + ye).powf(self.inv_exponent))
        } else {
            self.profile(x0.max(y0))
        }
    }

    /// Coverage at distance term `m`. Outside the corners the term is
    /// `max(x0, y0)`, so coverage there is this one-dimensional profile of
    /// the row's or the column's term.
    fn profile(&self, m: f32) -> f32 {
        let d = m - self.r1;
        self.scale * (erf7(self.inv_std_dev * (self.min_edge + d)) - erf7(self.inv_std_dev * d))
    }

    /// Half extents (in local units) of the region where the blur is constant.
    fn interior(&self) -> Option<(f32, f32)> {
        const SATURATED: f32 = 3.5;
        let half_min = 0.5 * self.width.min(self.height);
        if self.min_edge - half_min < SATURATED * self.std_dev {
            return None;
        }
        let m = (self.r1 - SATURATED * self.std_dev).min(0.0);
        let x = self.width * 0.5 - self.r1 + m;
        let y = self.height * 0.5 - self.r1 + m;
        (x > 0.0 && y > 0.0).then_some((x, y))
    }

    fn interior_alpha(&self) -> f32 {
        self.scale * (erf7(100.0) - erf7(-100.0))
    }
}

/// vello's fine-shader erf approximation.
fn erf7(x: f32) -> f32 {
    let y = (x * std::f32::consts::FRAC_2_SQRT_PI).clamp(-100.0, 100.0);
    let yy = y * y;
    let z = y + (0.24295 + (0.03395 + 0.0104 * yy) * yy) * (y * yy);
    z / (1.0 + z * z).sqrt()
}

/// `path` under `transform` as an axis-aligned device rectangle, if it is one.
pub(crate) fn as_device_rect(path: &BezPath, transform: Affine) -> Option<Rect> {
    let mut pts: [Point; 5] = [Point::ZERO; 5];
    let mut n = 0;
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) if n < 5 => {
                pts[n] = transform * p;
                n += 1;
            }
            PathEl::ClosePath => {}
            _ => return None,
        }
    }
    if n == 5 && pts[4].distance(pts[0]) < 1e-6 {
        n = 4;
    }
    if n != 4 {
        return None;
    }
    let rect = Rect::from_points(pts[0], pts[2]);
    let on_corner = |p: Point| {
        (p.x - rect.x0).abs().min((p.x - rect.x1).abs()) < 1e-6
            && (p.y - rect.y0).abs().min((p.y - rect.y1).abs()) < 1e-6
    };
    let axis_aligned = pts.iter().take(4).all(|&p| on_corner(p))
        && pts[0].x != pts[2].x
        && pts[0].y != pts[2].y
        && (pts[0].x == pts[1].x || pts[0].y == pts[1].y);
    axis_aligned.then_some(rect)
}

pub(crate) struct BlurDraw<'a> {
    pub shape: &'a BezPath,
    pub transform: Affine,
    pub brush_transform: Affine,
    pub params: BlurParams,
    pub color: AlphaColor<Srgb>,
    pub bbox: Rect,
}

/// Draw one blurred rounded rectangle; `offset` maps the scene into the
/// region being recorded. Returns whether a mask had to be computed.
pub(crate) fn draw(
    ctx: &mut RenderContext,
    cache: &mut BlurCache,
    cached: bool,
    offset: Affine,
    blur: &BlurDraw<'_>,
) -> bool {
    let [sx, b, c, sy, tx, ty] = blur.brush_transform.as_coeffs();
    let axis_aligned = b == 0.0 && c == 0.0 && sx != 0.0 && sy != 0.0;
    if !cached || !axis_aligned {
        draw_direct(ctx, offset, blur);
        return false;
    }
    let (sx, sy) = (sx.abs(), sy.abs());
    // Snap the blur origin to an eighth of a pixel; the mask grid is the
    // device pixel grid.
    let qx = (tx * PHASES).round() / PHASES;
    let qy = (ty * PHASES).round() / PHASES;
    let (ox, oy) = (qx.floor(), qy.floor());
    let phase = [((qx - ox) * PHASES) as i8, ((qy - oy) * PHASES) as i8];
    let bbox = blur.bbox;
    let extents = [
        (bbox.x0.floor() - ox) as i32,
        (bbox.y0.floor() - oy) as i32,
        (bbox.x1.ceil() - ox) as i32,
        (bbox.y1.ceil() - oy) as i32,
    ];
    let (mw, mh) = (extents[2] - extents[0], extents[3] - extents[1]);
    if mw <= 0
        || mh <= 0
        || mw > i32::from(u16::MAX)
        || mh > i32::from(u16::MAX)
        || (mw as usize) * (mh as usize) > MAX_MASK_PIXELS
    {
        draw_direct(ctx, offset, blur);
        return false;
    }
    let key = Key {
        params: [
            blur.params.width.to_bits(),
            blur.params.height.to_bits(),
            blur.params.radius.to_bits(),
            blur.params.std_dev.to_bits(),
        ],
        scale: [(sx as f32).to_bits(), (sy as f32).to_bits()],
        phase,
        extents,
    };
    let frame = cache.frame;
    let mut missed = false;
    let mask = cache.masks.entry(key).or_insert_with(|| {
        missed = true;
        build_mask(blur.params, sx, sy, qx - ox, qy - oy, extents)
    });
    mask.last_used = frame;

    // Mask pixel (0, 0) is device pixel (ox + extents[0], oy + extents[1]).
    let (mx, my) = (ox + f64::from(extents[0]), oy + f64::from(extents[1]));
    let interior = mask.interior.map(|(x0, y0, x1, y1)| {
        Rect::new(
            mx + f64::from(x0),
            my + f64::from(y0),
            mx + f64::from(x1),
            my + f64::from(y1),
        )
    });
    let shape_rect = as_device_rect(blur.shape, blur.transform);
    let interior = interior.and_then(|interior| {
        let shape = shape_rect?;
        let inside = interior.x0 >= shape.x0
            && interior.y0 >= shape.y0
            && interior.x1 <= shape.x1
            && interior.y1 <= shape.y1;
        inside.then_some((shape, interior))
    });

    let color = blur.color;
    ctx.set_transform(offset);
    ctx.reset_paint_transform();
    let path = &mut cache.scratch;
    path.truncate(0);
    let fill = if let Some((shape, interior)) = interior {
        // Solid interior, then the edge band around it.
        ctx.set_paint(color.multiply_alpha(mask.interior_alpha));
        ctx.fill_rect(&interior);
        push_rect(path, shape);
        push_rect(path, interior);
        Fill::EvenOdd
    } else {
        for el in blur.shape.elements() {
            path.push(blur.transform * *el);
        }
        Fill::NonZero
    };
    ctx.set_fill_rule(fill);
    ctx.set_paint(Image {
        image: ImageSource::Pixmap(mask.pixmap.clone()),
        sampler: ImageSampler {
            x_extend: Extend::Pad,
            y_extend: Extend::Pad,
            quality: ImageQuality::Low,
            alpha: 1.0,
        },
    });
    ctx.set_tint(Some(Tint {
        color,
        mode: TintMode::AlphaMask,
    }));
    ctx.set_paint_transform(Affine::translate((mx, my)));
    ctx.fill_path(path);
    ctx.reset_tint();
    ctx.reset_paint_transform();
    missed
}

fn push_rect(path: &mut BezPath, r: Rect) {
    path.move_to((r.x0, r.y0));
    path.line_to((r.x1, r.y0));
    path.line_to((r.x1, r.y1));
    path.line_to((r.x0, r.y1));
    path.close_path();
}

/// Evaluate the blur per pixel through `vello_cpu`, clipped to the shape when
/// the shape is not vello's default footprint.
fn draw_direct(ctx: &mut RenderContext, offset: Affine, blur: &BlurDraw<'_>) {
    let p = blur.params;
    let half = Rect::new(
        -0.5 * f64::from(p.width),
        -0.5 * f64::from(p.height),
        0.5 * f64::from(p.width),
        0.5 * f64::from(p.height),
    );
    let kernel = 2.5 * f64::from(p.std_dev);
    let footprint = blur
        .brush_transform
        .transform_rect_bbox(half.inflate(kernel, kernel));
    let clip = !as_device_rect(blur.shape, blur.transform).is_some_and(|r| {
        (r.x0 - footprint.x0).abs() < 1e-3
            && (r.y0 - footprint.y0).abs() < 1e-3
            && (r.x1 - footprint.x1).abs() < 1e-3
            && (r.y1 - footprint.y1).abs() < 1e-3
    });
    if clip {
        ctx.set_transform(offset * blur.transform);
        ctx.set_fill_rule(Fill::NonZero);
        ctx.push_clip_layer(blur.shape);
    }
    ctx.set_transform(offset * blur.brush_transform);
    // vello evaluates the blur at pixel corners, vello_cpu at centres.
    let corner =
        blur.brush_transform.inverse() * Affine::translate((0.5, 0.5)) * blur.brush_transform;
    ctx.set_paint_transform(corner);
    ctx.set_paint(blur.color);
    ctx.fill_blurred_rounded_rect(&half, p.radius, p.std_dev, false);
    if clip {
        ctx.pop_layer();
    }
}

/// Coverage of the blur with origin at sub-pixel phase `(fx, fy)` of device
/// pixel `(0, 0)`, over mask pixels `extents` relative to that pixel.
fn build_mask(p: BlurParams, sx: f64, sy: f64, fx: f64, fy: f64, extents: [i32; 4]) -> Mask {
    let shape = Shape::new(p);
    let (w, h) = (
        (extents[2] - extents[0]) as usize,
        (extents[3] - extents[1]) as usize,
    );
    // vello's fine shader evaluates the blur at integer pixel coordinates.
    let local = |pixel: usize, origin: i32, phase: f64, scale: f64| {
        ((f64::from(pixel as i32 + origin) - phase) / scale) as f32
    };
    let to_byte = |a: f32| (a * 255.0).round().clamp(0.0, 255.0) as u8;
    // Per column: the distance term, its power for the corners, and the
    // profile used wherever the column's term is the larger one.
    let xs: Vec<(f32, f32, u8)> = (0..w)
        .map(|i| {
            let x0 = shape.axis(local(i, extents[0], fx, sx), shape.width);
            let xe = if x0 > 0.0 {
                x0.powf(shape.exponent)
            } else {
                0.0
            };
            (x0, xe, to_byte(shape.profile(x0)))
        })
        .collect();
    let interior = shape.interior().and_then(|(lx, ly)| {
        let cols: Vec<usize> = (0..w)
            .filter(|&i| local(i, extents[0], fx, sx).abs() <= lx)
            .collect();
        let rows: Vec<usize> = (0..h)
            .filter(|&j| local(j, extents[1], fy, sy).abs() <= ly)
            .collect();
        let (x0, x1) = (*cols.first()?, *cols.last()?);
        let (y0, y1) = (*rows.first()?, *rows.last()?);
        Some((x0 as i32, y0 as i32, x1 as i32 + 1, y1 as i32 + 1))
    });
    let interior_alpha = shape.interior_alpha();
    let interior_byte = (interior_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    let mut data = vec![0u8; w * h * 4];
    for (j, row) in data.chunks_exact_mut(w * 4).enumerate() {
        let y0 = shape.axis(local(j, extents[1], fy, sy), shape.height);
        let ye = if y0 > 0.0 {
            y0.powf(shape.exponent)
        } else {
            0.0
        };
        let row_byte = to_byte(shape.profile(y0));
        let skip = interior
            .filter(|&(_, y_lo, _, y_hi)| (j as i32) >= y_lo && (j as i32) < y_hi)
            .map(|(x_lo, _, x_hi, _)| (x_lo as usize, x_hi as usize));
        for (i, (px, &(x0, xe, col_byte))) in row.chunks_exact_mut(4).zip(&xs).enumerate() {
            let a = if skip.is_some_and(|(lo, hi)| i >= lo && i < hi) {
                interior_byte
            } else if x0 > 0.0 && y0 > 0.0 {
                to_byte(shape.alpha(x0, xe, y0, ye))
            } else if x0 >= y0 {
                col_byte
            } else {
                row_byte
            };
            px.copy_from_slice(&[a, a, a, a]);
        }
    }
    let pixmap = Pixmap::from_parts(
        data,
        w as u16,
        h as u16,
        PixelMetadata::new(vello_cpu::peniko::ImageAlphaType::AlphaPremultiplied, true),
    );
    Mask {
        pixmap: Arc::new(pixmap),
        interior,
        interior_alpha,
        last_used: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erf7_is_odd_and_saturates() {
        assert!((erf7(0.0)).abs() < 1e-6);
        assert!((erf7(1.0) + erf7(-1.0)).abs() < 1e-6);
        assert!(erf7(4.0) > 0.9999);
    }

    #[test]
    fn interior_alpha_matches_the_formula_deep_inside() {
        let shape = Shape::new(BlurParams {
            width: 400.0,
            height: 300.0,
            radius: 12.0,
            std_dev: 8.0,
        });
        let (lx, ly) = shape.interior().unwrap_or_default();
        assert!(lx > 100.0 && ly > 100.0, "{lx} {ly}");
        for (x, y) in [(0.0, 0.0), (lx, 0.0), (0.0, ly), (lx, ly)] {
            let x0 = shape.axis(x, shape.width);
            let y0 = shape.axis(y, shape.height);
            let a = shape.alpha(x0, 0.0, y0, 0.0);
            assert!(
                (a - shape.interior_alpha()).abs() < 0.5 / 255.0,
                "{x},{y}: {a}"
            );
        }
    }

    #[test]
    fn device_rects_are_recognised() {
        let mut p = BezPath::new();
        push_rect(&mut p, Rect::new(1.0, 2.0, 5.0, 7.0));
        assert_eq!(
            as_device_rect(&p, Affine::translate((10.0, 0.0))),
            Some(Rect::new(11.0, 2.0, 15.0, 7.0))
        );
        assert_eq!(as_device_rect(&p, Affine::rotate(0.3)), None);
    }
}
