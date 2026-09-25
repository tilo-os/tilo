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
pub(crate) fn palette() -> (Color, Color) {
    let colors = &ui::theme::Theme::dark().tokens.colors;
    (colors.text_primary, colors.surfaces.base)
}

/// Largest legacy KMS cursor plane supported by every backend we target.
pub const PLANE_SIZE: u32 = 64;
/// Side of the square grid the cursor outline is authored on.
const DESIGN_SIZE: u32 = 24;
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
    let scale = f64::from(size) / f64::from(DESIGN_SIZE);
    let transform = Affine::translate((hotspot.x, hotspot.y)).then_scale(scale);
    draw_vector(scene, shape, transform, scale.recip());
}

fn default_path() -> BezPath {
    // A compact, slightly softened modern arrow. The tip is deliberately
    // (0,0), matching the explicit hotspot and existing pointer coordinates.
    const OUTLINE: [(f64, f64); 7] = [
        (1.0, 0.75),
        (1.2, 19.15),
        (5.8, 14.75),
        (9.65, 22.25),
        (13.25, 20.35),
        (9.45, 13.1),
        (16.05, 12.7),
    ];
    let mut path = BezPath::new();
    let mut points = OUTLINE.into_iter();
    if let Some(start) = points.next() {
        path.move_to(start);
    }
    for point in points {
        path.line_to(point);
    }
    path.close_path();
    path
}

fn draw_vector(scene: &mut Scene, shape: CursorShape, transform: Affine, inverse_scale: f64) {
    let path = match shape {
        CursorShape::Default => default_path(),
    };
    // Scale stroke back into design space so its final physical width stays
    // close to two pixels at all supported accessibility sizes.
    // The system pointer deliberately keeps canonical white fill and black
    // outline across themes so it remains identifiable over arbitrary client
    // content, which is not controlled by the shell theme.
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
    let scale = f64::from(size) / f64::from(DESIGN_SIZE);
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
            let offset = y.saturating_mul(size).saturating_add(x).saturating_mul(4);
            let offset = usize::try_from(offset).unwrap_or(usize::MAX);
            if let Some(pixel) = rgba.get_mut(offset..offset.saturating_add(4)) {
                let [r, g, b] = color;
                pixel.copy_from_slice(&[r, g, b, alpha]);
            }
        }
    }
    rgba
}

/// Each polygon vertex paired with the one before it, wrapping around.
fn edges(polygon: &[(f64, f64)]) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
    let previous = polygon.last().into_iter().chain(polygon.iter());
    polygon.iter().copied().zip(previous.copied())
}

fn point_in_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    for (current, previous) in edges(polygon) {
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
    edges(polygon)
        .map(|(current, previous)| segment_distance(point, previous, current))
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

/// The four bytes at `offset`, when the whole pixel lies inside `buffer`.
fn pixel_at(buffer: &[u8], offset: usize) -> Option<[u8; 4]> {
    buffer.get(offset..offset.checked_add(4)?)?.try_into().ok()
}

/// `channel * alpha / 255`, the premultiplication of one 8-bit channel.
fn premultiply(channel: u8, alpha: u8) -> u32 {
    u32::from(channel).saturating_mul(u32::from(alpha)) / 255
}

fn downsample_premultiplied(src: &[u8], source_size: u32, size: u32) -> Vec<u8> {
    let mut out =
        vec![0_u8; usize::try_from(size.saturating_mul(size).saturating_mul(4)).unwrap_or(0)];
    let ss = usize::try_from(SUPERSAMPLE).unwrap_or(1);
    let source_size = usize::try_from(source_size).unwrap_or(0);
    let size = usize::try_from(size).unwrap_or(0);
    let samples = u32::try_from(ss.saturating_mul(ss)).unwrap_or(1).max(1);
    for (index, dst) in out.chunks_exact_mut(4).enumerate() {
        let (dst_row, dst_column) = (
            index.checked_div(size).unwrap_or(0),
            index.checked_rem(size).unwrap_or(0),
        );
        let mut sum = [0_u32; 4];
        for sy in 0..ss {
            for sx in 0..ss {
                let row = dst_row.saturating_mul(ss).saturating_add(sy);
                let column = dst_column.saturating_mul(ss).saturating_add(sx);
                let src_offset = row
                    .saturating_mul(source_size)
                    .saturating_add(column)
                    .saturating_mul(4);
                let Some([red, green, blue, alpha]) = pixel_at(src, src_offset) else {
                    continue;
                };
                let [red_sum, green_sum, blue_sum, alpha_sum] = &mut sum;
                *red_sum = red_sum.saturating_add(premultiply(red, alpha));
                *green_sum = green_sum.saturating_add(premultiply(green, alpha));
                *blue_sum = blue_sum.saturating_add(premultiply(blue, alpha));
                *alpha_sum = alpha_sum.saturating_add(u32::from(alpha));
            }
        }
        for (byte, total) in dst.iter_mut().zip(sum) {
            *byte = u8::try_from(total.checked_div(samples).unwrap_or(0).min(255)).unwrap_or(255);
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
            let src_offset = y
                .saturating_mul(cursor.width)
                .saturating_add(x)
                .saturating_mul(4);
            let src_offset = usize::try_from(src_offset).unwrap_or(usize::MAX);
            let Some([red, green, blue, alpha]) = pixel_at(&cursor.rgba, src_offset) else {
                continue;
            };
            if alpha == 0 {
                continue;
            }
            let dst_offset = dy
                .saturating_mul(pitch)
                .saturating_add(dx.saturating_mul(4));
            let Some(pixel) = dst.get_mut(dst_offset..dst_offset.saturating_add(4)) else {
                continue;
            };
            let Ok([dst_blue, dst_green, dst_red, dst_x]) = <&mut [u8; 4]>::try_from(pixel) else {
                continue;
            };
            let inverse = 255_u8.saturating_sub(alpha);
            let over = |source: u8, under: u8| {
                u8::try_from(
                    u32::from(source)
                        .saturating_add(premultiply(under, inverse))
                        .min(255),
                )
                .unwrap_or(u8::MAX)
            };
            // XRGB is little-endian B,G,R,X; the source is premultiplied RGBA.
            *dst_blue = over(blue, *dst_blue);
            *dst_green = over(green, *dst_green);
            *dst_red = over(red, *dst_red);
            *dst_x = 0xff;
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
