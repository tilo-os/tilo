//! One source of truth for the compositor-owned pointer.
//!
//! The shape is stored as vector geometry in a 24-unit design space and
//! rasterised with supersampling. DRM cursor planes, the software fallback,
//! and captures all consume this same bitmap, so changing render paths never
//! changes the pointer's silhouette or hotspot.

use std::sync::{Arc, Mutex, OnceLock};

use ui::ColorCompat;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Stroke};
use vello::peniko::{Color, Fill};

/// Cursor ink comes from the shared dark desktop theme rather than bypassing
/// the design-token contract.  Keeping the pair here also makes the hardware
/// plane, software fallback, and captured pointer use identical colours.
fn palette() -> (Color, Color) {
    let colors = &ui::theme::Theme::dark().tokens.colors;
    (colors.text_primary, colors.surfaces.base)
}

/// Largest legacy KMS cursor plane supported by every backend we target.
pub const PLANE_SIZE: u32 = 64;
const DESIGN_SIZE: f64 = 24.0;
const SUPERSAMPLE: u32 = 4;

/// Shapes owned by the compositor. More named shapes can be added without
/// changing the transport used by the scanout backends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CursorShape {
    #[default]
    Default,
}

/// Premultiplied RGBA cursor pixels and their explicit hotspot.
#[derive(Clone, Debug)]
pub struct CursorBitmap {
    pub width: u32,
    pub height: u32,
    pub hotspot: (u32, u32),
    rgba: Arc<[u8]>,
}

impl CursorBitmap {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

type Cache = Vec<((CursorShape, u32), Arc<CursorBitmap>)>;

/// Rasterise once per shape/size. Size is clamped to the KMS plane contract.
pub fn bitmap(shape: CursorShape, size: u32) -> Arc<CursorBitmap> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let size = size.clamp(24, PLANE_SIZE);
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, found)) = cache.iter().find(|(key, _)| *key == (shape, size)) {
        return Arc::clone(found);
    }
    let rendered = Arc::new(rasterise(shape, size));
    cache.push(((shape, size), Arc::clone(&rendered)));
    rendered
}

/// Paint the shared cursor into a scene at its hotspot. Capture uses this
/// rather than maintaining a second approximation of the pointer.
pub fn draw_scene(scene: &mut Scene, shape: CursorShape, size: u32, hotspot: vello::kurbo::Point) {
    let size = size.clamp(24, PLANE_SIZE);
    let scale = f64::from(size) / DESIGN_SIZE;
    let transform = Affine::translate((hotspot.x, hotspot.y)).then_scale(scale);
    draw_vector(scene, shape, transform, scale.recip());
}

fn default_path() -> BezPath {
    // A compact, slightly softened modern arrow. The tip is deliberately
    // (0,0), matching the explicit hotspot and existing pointer coordinates.
    BezPath::from_svg(
        "M1 0.75 L1.2 19.15 L5.8 14.75 L9.65 22.25 L13.25 20.35 L9.45 13.1 L16.05 12.7 Z",
    )
    .expect("built-in cursor path must parse")
}

fn draw_vector(scene: &mut Scene, shape: CursorShape, transform: Affine, inverse_scale: f64) {
    let path = match shape {
        CursorShape::Default => default_path(),
    };
    // Scale stroke back into design space so its final physical width stays
    // close to two pixels at all supported accessibility sizes.
    let stroke = Stroke::new(2.0 * inverse_scale);
    let (fill, outline) = palette();
    scene.fill(Fill::NonZero, transform, fill, None, &path);
    scene.stroke(&stroke, transform, outline, None, &path);
}

fn rasterise(shape: CursorShape, size: u32) -> CursorBitmap {
    let supersampled = size.saturating_mul(SUPERSAMPLE);
    let high = rasterise_cpu(shape, supersampled);
    let rgba = downsample_premultiplied(&high, supersampled, size);
    CursorBitmap {
        width: size,
        height: size,
        hotspot: (0, 0),
        rgba: Arc::from(rgba),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "cursor coverage and coordinates are bounded to a 256px supersampled plane"
)]
fn rasterise_cpu(shape: CursorShape, size: u32) -> Vec<u8> {
    let mut rgba =
        vec![0_u8; usize::try_from(size.saturating_mul(size).saturating_mul(4)).unwrap_or(0)];
    let scale = f64::from(size) / DESIGN_SIZE;
    let path = match shape {
        CursorShape::Default => default_path(),
    };
    let outline_points = path
        .elements()
        .iter()
        .filter_map(|element| match element {
            vello::kurbo::PathEl::MoveTo(point) | vello::kurbo::PathEl::LineTo(point) => {
                Some((point.x * scale, point.y * scale))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let stroke_radius = (f64::from(SUPERSAMPLE) * 0.85).max(1.0);
    let (fill, outline_color) = palette();
    for y in 0..size {
        for x in 0..size {
            let point = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let inside = point_in_polygon(point, &outline_points);
            let edge_distance = polygon_distance(point, &outline_points);
            let (color, alpha) = if edge_distance <= stroke_radius {
                (
                    [outline_color.r(), outline_color.g(), outline_color.b()],
                    outline_color.a(),
                )
            } else if inside {
                ([fill.r(), fill.g(), fill.b()], fill.a())
            } else {
                ([0_u8, 0, 0], 0_u8)
            };
            let offset = usize::try_from((y * size + x) * 4).unwrap_or(usize::MAX);
            if let Some(pixel) = rgba.get_mut(offset..offset.saturating_add(4)) {
                pixel.copy_from_slice(&[color[0], color[1], color[2], alpha]);
            }
        }
    }
    rgba
}

fn point_in_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let current = polygon[index];
        let previous = polygon[(index + polygon.len() - 1) % polygon.len()];
        if (current.1 > point.1) != (previous.1 > point.1)
            && point.0
                < (previous.0 - current.0) * (point.1 - current.1) / (previous.1 - current.1)
                    + current.0
        {
            inside = !inside;
        }
    }
    inside
}

fn polygon_distance(point: (f64, f64), polygon: &[(f64, f64)]) -> f64 {
    (0..polygon.len())
        .map(|index| segment_distance(point, polygon[index], polygon[(index + 1) % polygon.len()]))
        .fold(f64::INFINITY, f64::min)
}

fn segment_distance(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> f64 {
    let delta = (end.0 - start.0, end.1 - start.1);
    let length_squared = delta.0 * delta.0 + delta.1 * delta.1;
    if length_squared <= f64::EPSILON {
        return ((point.0 - start.0).powi(2) + (point.1 - start.1).powi(2)).sqrt();
    }
    let projection = (((point.0 - start.0) * delta.0 + (point.1 - start.1) * delta.1)
        / length_squared)
        .clamp(0.0, 1.0);
    let nearest = (
        start.0 + projection * delta.0,
        start.1 + projection * delta.1,
    );
    ((point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2)).sqrt()
}

fn downsample_premultiplied(src: &[u8], source_size: u32, size: u32) -> Vec<u8> {
    let mut out =
        vec![0_u8; usize::try_from(size.saturating_mul(size).saturating_mul(4)).unwrap_or(0)];
    let ss = usize::try_from(SUPERSAMPLE).unwrap_or(1);
    let source_size = usize::try_from(source_size).unwrap_or(0);
    let size = usize::try_from(size).unwrap_or(0);
    let samples = u32::try_from(ss.saturating_mul(ss)).unwrap_or(1);
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0_u32; 4];
            for sy in 0..ss {
                for sx in 0..ss {
                    let src_offset = ((y * ss + sy) * source_size + x * ss + sx) * 4;
                    let Some(pixel) = src.get(src_offset..src_offset + 4) else {
                        continue;
                    };
                    let alpha = u32::from(pixel[3]);
                    sum[0] = sum[0].saturating_add(u32::from(pixel[0]) * alpha / 255);
                    sum[1] = sum[1].saturating_add(u32::from(pixel[1]) * alpha / 255);
                    sum[2] = sum[2].saturating_add(u32::from(pixel[2]) * alpha / 255);
                    sum[3] = sum[3].saturating_add(alpha);
                }
            }
            let dst_offset = (y * size + x) * 4;
            for channel in 0..4 {
                out[dst_offset + channel] =
                    u8::try_from((sum[channel] / samples).min(255)).unwrap_or(255);
            }
        }
    }
    out
}

/// Alpha-composite the shared premultiplied bitmap into an XRGB8888 buffer.
pub fn composite_xrgb(
    dst: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    px: i32,
    py: i32,
    cursor: &CursorBitmap,
) {
    let hotspot_x = i32::try_from(cursor.hotspot.0).unwrap_or(0);
    let hotspot_y = i32::try_from(cursor.hotspot.1).unwrap_or(0);
    for y in 0..cursor.height {
        for x in 0..cursor.width {
            let dx = px
                .saturating_sub(hotspot_x)
                .saturating_add(i32::try_from(x).unwrap_or(i32::MAX));
            let dy = py
                .saturating_sub(hotspot_y)
                .saturating_add(i32::try_from(y).unwrap_or(i32::MAX));
            let (Ok(dx), Ok(dy)) = (usize::try_from(dx), usize::try_from(dy)) else {
                continue;
            };
            if dx >= width || dy >= height {
                continue;
            }
            let src_offset = usize::try_from((y * cursor.width + x) * 4).unwrap_or(usize::MAX);
            let Some(src) = cursor.rgba.get(src_offset..src_offset.saturating_add(4)) else {
                continue;
            };
            let alpha = u32::from(src[3]);
            if alpha == 0 {
                continue;
            }
            let dst_offset = dy
                .saturating_mul(pitch)
                .saturating_add(dx.saturating_mul(4));
            let Some(pixel) = dst.get_mut(dst_offset..dst_offset.saturating_add(4)) else {
                continue;
            };
            let inverse = 255_u32.saturating_sub(alpha);
            // XRGB is little-endian B,G,R,X; the source is premultiplied RGBA.
            pixel[0] =
                u8::try_from((u32::from(src[2]) + u32::from(pixel[0]) * inverse / 255).min(255))
                    .unwrap_or(255);
            pixel[1] =
                u8::try_from((u32::from(src[1]) + u32::from(pixel[1]) * inverse / 255).min(255))
                    .unwrap_or(255);
            pixel[2] =
                u8::try_from((u32::from(src[0]) + u32::from(pixel[2]) * inverse / 255).min(255))
                    .unwrap_or(255);
            pixel[3] = 0xff;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cursor_is_cached_and_antialiased_at_accessibility_sizes() {
        for size in [24, 48, 64] {
            let first = bitmap(CursorShape::Default, size);
            let second = bitmap(CursorShape::Default, size);
            assert!(Arc::ptr_eq(&first, &second));
            assert_eq!((first.width, first.height), (size, size));
            assert_eq!(first.hotspot, (0, 0));
            assert_eq!(first.rgba.len(), usize::try_from(size * size * 4).unwrap());
            assert!(first.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
            assert!(
                first
                    .rgba
                    .chunks_exact(4)
                    .any(|pixel| (1..=254).contains(&pixel[3]))
            );
            assert!(first.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        }
    }

    #[test]
    fn software_composite_clips_and_preserves_uncovered_pixels() {
        let cursor = bitmap(CursorShape::Default, 24);
        let mut dst = vec![17_u8; 12 * 12 * 4];
        composite_xrgb(&mut dst, 12 * 4, 12, 12, -3, -3, &cursor);
        assert!(dst.chunks_exact(4).any(|pixel| pixel != [17, 17, 17, 17]));
        assert_eq!(&dst[(11 * 12 + 11) * 4..][..4], &[17, 17, 17, 17]);
    }
}
