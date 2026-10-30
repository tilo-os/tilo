//! What a window looks like on the desktop: rounded corners, the soft
//! shadow it lifts off the ground with and the hairline along its edge.
//!
//! A client buffer is square, so the shape around it is the compositor's.
//! Two pieces make it, both of them plain data the session sets
//! ([`crate::Compositor::set_window_frame`]):
//!
//! - The shadow and the edge are one image, drawn on the CPU once per
//!   window size: a rounded rect blurred with a Gaussian and a stroke just
//!   outside its outline, premultiplied and antialiased. It never changes
//!   while the window keeps its size, so it costs one upload and is then an
//!   ordinary buffer in front of the window's own.
//! - The corners are where the window's buffer is not drawn at all
//!   ([`bands`]): the rows of the corner strips, each cut to the arc. The
//!   rows are computed in the output's pixels after the window's effect is
//!   applied, so a window that is moved, scaled or faded by its effect keeps
//!   its corners — and a window pictured small gets few, small rows.
//!
//! All of it is measured in logical pixels and scaled by the output, as the
//! design language's tokens are: [`WindowFrame::default`] is the dark
//! theme's window tile (radius 14, a 28 % black shadow 2 below it blurred by
//! 16, a 1 px white hairline at 14 %), which is what the shell drew around
//! every app.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use std::cell::RefCell;
use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Physical, Rectangle, Transform};

/// A drop shadow: the outline of the window, blurred, offset down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// How far below the window it sits, logical pixels.
    pub offset_y: f64,
    /// Its blur radius, logical pixels (a Gaussian of half that as its
    /// standard deviation, as CSS and the design tokens mean it).
    pub blur: f64,
    /// Its colour, straight alpha.
    pub rgba: [u8; 4],
}

/// The hairline that marks where a window ends, drawn just outside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// Its width, logical pixels.
    pub width: f64,
    /// Its colour, straight alpha.
    pub rgba: [u8; 4],
}

/// How every window is drawn on the desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowFrame {
    /// Corner radius, logical pixels; zero for square corners.
    pub radius: f64,
    /// The shadow under the window, if it casts one.
    pub shadow: Option<Shadow>,
    /// The hairline around the window, if it has one.
    pub edge: Option<Edge>,
}

impl WindowFrame {
    /// No corners, no shadow, no edge: a window drawn as its buffer is
    /// (a window that fills the display, which the display can scan out).
    #[must_use]
    pub const fn square() -> Self {
        Self {
            radius: 0.0,
            shadow: None,
            edge: None,
        }
    }

    /// Whether this draws nothing around a window.
    #[must_use]
    pub fn is_square(&self) -> bool {
        self.radius <= 0.0
            && self.shadow.is_none_or(|shadow| shadow.rgba[3] == 0)
            && self
                .edge
                .is_none_or(|edge| edge.rgba[3] == 0 || edge.width <= 0.0)
    }
}

impl Default for WindowFrame {
    /// The window tile the design language draws: what every app had around
    /// it before it was a foreign window.
    fn default() -> Self {
        Self {
            radius: 14.0,
            shadow: Some(Shadow {
                offset_y: 2.0,
                blur: 16.0,
                rgba: [0, 0, 0, 71],
            }),
            edge: Some(Edge {
                width: 1.0,
                rgba: [255, 255, 255, 36],
            }),
        }
    }
}

/// The shadow reaches this many standard deviations before it is cut, where
/// the toolkit's blur stops painting it too.
const REACH: f64 = 2.5;

/// The most rows a corner is cut into, however large it is drawn: past this
/// the rows grow rather than multiply, so a huge scaled window stays a few
/// dozen elements.
const MOST_ROWS: i32 = 32;

/// One window's shadow and edge, drawn once and drawn again every frame.
pub(crate) struct FrameImage {
    pub(crate) buffer: MemoryRenderBuffer,
    /// Where its top left sits relative to the window's, physical pixels.
    pub(crate) origin: (f64, f64),
    /// Its size, physical pixels.
    pub(crate) size: (f64, f64),
}

/// The frame images drawn so far, by what they were drawn from.
#[derive(Default)]
pub(crate) struct FrameImages {
    images: RefCell<Images>,
}

/// The images, and how many bytes of window frame they hold.
#[derive(Default)]
struct Images {
    by_signature: HashMap<Signature, std::rc::Rc<FrameImage>>,
    bytes: usize,
}

/// The most memory a desktop's window frames may hold on to: a handful of
/// window sizes, and a start over when a session keeps resizing.
const BUDGET: usize = 64 * 1024 * 1024;

/// What a frame image was drawn from: the same numbers always give the same
/// image.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Signature {
    /// Window size and corner radius in 1/64 pixels.
    size: (u32, u32),
    radius: u32,
    shadow: Option<(u32, u32, [u8; 4])>,
    edge: Option<(u32, [u8; 4])>,
}

/// `value` in 1/64 pixels, never negative.
fn sixtyfourths(value: f64) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a length on a screen, clamped positive and quantized to 1/64 px"
    )]
    let quantized = (value.max(0.0) * 64.0).round() as u32;
    quantized
}

impl FrameImages {
    /// The shadow and edge of a window `size` (logical) with `frame`, on an
    /// output of `scale`; `None` when the frame draws nothing.
    pub(crate) fn image(
        &self,
        frame: &WindowFrame,
        size: (f64, f64),
        scale: f64,
    ) -> Option<std::rc::Rc<FrameImage>> {
        if frame.is_square() || size.0 <= 0.0 || size.1 <= 0.0 || scale <= 0.0 {
            return None;
        }
        // The image is drawn in the output's pixels: the signature says so.
        let signature = Signature {
            size: (sixtyfourths(size.0 * scale), sixtyfourths(size.1 * scale)),
            radius: sixtyfourths(frame.radius * scale),
            shadow: frame
                .shadow
                .filter(|shadow| shadow.rgba[3] > 0)
                .map(|shadow| {
                    (
                        sixtyfourths(shadow.offset_y * scale),
                        sixtyfourths(shadow.blur * scale),
                        shadow.rgba,
                    )
                }),
            edge: frame
                .edge
                .filter(|edge| edge.rgba[3] > 0 && edge.width > 0.0)
                .map(|edge| (sixtyfourths(edge.width * scale), edge.rgba)),
        };
        if let Some(image) = self.images.borrow().by_signature.get(&signature) {
            return Some(std::rc::Rc::clone(image));
        }
        let raster = Raster::of(&signature)?;
        let bytes = raster.rgba.len();
        let image = std::rc::Rc::new(FrameImage {
            // RGBA bytes are ABGR8888 in DRM's little-endian naming.
            buffer: MemoryRenderBuffer::from_slice(
                &raster.rgba,
                Fourcc::Abgr8888,
                (i32_from(raster.texels.0), i32_from(raster.texels.1)),
                1,
                Transform::Normal,
                None,
            ),
            origin: raster.origin,
            size: raster.size,
        });
        let mut images = self.images.borrow_mut();
        // A desktop's windows come in a handful of sizes; a long-lived
        // session resizing every window once should not keep every size it
        // ever saw.
        if images.bytes.saturating_add(bytes) > BUDGET {
            images.by_signature.clear();
            images.bytes = 0;
        }
        images.bytes = images.bytes.saturating_add(bytes);
        images
            .by_signature
            .insert(signature, std::rc::Rc::clone(&image));
        Some(image)
    }
}

/// The premultiplied pixels of a frame, before they are a buffer.
struct Raster {
    rgba: Vec<u8>,
    texels: (u32, u32),
    /// Where its top left sits relative to the window's, physical pixels.
    origin: (f64, f64),
    /// Its size, physical pixels.
    size: (f64, f64),
}

impl Raster {
    /// Paint the shadow and edge a [`Signature`] describes.
    fn of(signature: &Signature) -> Option<Self> {
        let to_px = |quantized: u32| f64::from(quantized) / 64.0;
        let width = to_px(signature.size.0);
        let height = to_px(signature.size.1);
        if width < 1.0 || height < 1.0 {
            return None;
        }
        let radius = to_px(signature.radius).clamp(0.0, width.min(height) / 2.0);
        let shadow = signature
            .shadow
            .map(|(offset, blur, rgba)| (to_px(offset), to_px(blur) / 2.0, rgba));
        let edge = signature.edge.map(|(width, rgba)| (to_px(width), rgba));
        let offset = shadow.map_or(0.0, |(offset, _, _)| offset);
        let sigma = shadow.map_or(0.0, |(_, sigma, _)| sigma);
        let stroke = edge.map_or(0.0, |(width, _)| width);
        let reach = (REACH * sigma).max(stroke).ceil() + 1.0;
        let left = reach;
        let top = reach + (-offset).max(0.0);
        let right = reach;
        let bottom = reach + offset.max(0.0);
        let texels = (
            u32_from(width + left + right),
            u32_from(height + top + bottom),
        );
        if texels.0 == 0 || texels.1 == 0 {
            return None;
        }
        let mut rgba = vec![0_u8; texels.0 as usize * texels.1 as usize * 4];
        let shadow_colour =
            shadow.map(|(_, _, rgba)| rgba.map(|channel| f64::from(channel) / 255.0));
        let edge_colour = edge.map(|(_, rgba)| rgba.map(|channel| f64::from(channel) / 255.0));
        // The Gaussian's spread: a deviation of `sigma` over a half-turn.
        let spread = sigma * std::f64::consts::SQRT_2;
        for row in 0..texels.1 {
            for column in 0..texels.0 {
                let x = f64::from(column) + 0.5 - left;
                let y = f64::from(row) + 0.5 - top;
                // Behind the window is the shadow's silhouette, offset down;
                // the edge follows the window's own. A shadow sits under the
                // window only, never above its top edge, and never inside it:
                // the window's own buffer is drawn there.
                let inside = distance(x, y, width, height, radius);
                let shadow_alpha = match (shadow_colour, shadow) {
                    (Some(colour), Some(_)) if y >= 0.0 && inside > -1.0 => {
                        let below = distance(x, y - offset, width, height, radius);
                        let covered = if spread > 0.0 {
                            0.5 * erfc(below / spread)
                        } else if below <= 0.0 {
                            1.0
                        } else {
                            0.0
                        };
                        covered * colour[3]
                    }
                    _ => 0.0,
                };
                let edge_alpha = match (edge_colour, edge) {
                    (Some(colour), Some((thickness, _))) => {
                        // Full across the stroke just outside the outline, one
                        // pixel of falloff either side of it.
                        let outside = if inside < 0.0 {
                            -inside
                        } else if inside > thickness {
                            inside - thickness
                        } else {
                            0.0
                        };
                        (0.5 - outside).clamp(0.0, 1.0) * colour[3]
                    }
                    _ => 0.0,
                };
                if shadow_alpha <= 0.0 && edge_alpha <= 0.0 {
                    continue;
                }
                // Premultiplied: the edge over the shadow, both over nothing.
                let mut out = [0.0_f64; 3];
                if let Some(colour) = shadow_colour {
                    for (channel, value) in out.iter_mut().zip(colour) {
                        *channel = value * shadow_alpha;
                    }
                }
                if let Some(colour) = edge_colour {
                    for (channel, value) in out.iter_mut().zip(colour) {
                        *channel = value * edge_alpha + *channel * (1.0 - edge_alpha);
                    }
                }
                let alpha = edge_alpha + shadow_alpha * (1.0 - edge_alpha);
                let at = ((row * texels.0 + column) * 4) as usize;
                let Some(pixel) = rgba.get_mut(at..at + 4) else {
                    continue;
                };
                for (slot, value) in pixel.iter_mut().zip(out) {
                    *slot = u8_from(value.clamp(0.0, 1.0) * 255.0);
                }
                if let Some(slot) = pixel.get_mut(3) {
                    *slot = u8_from(alpha.clamp(0.0, 1.0) * 255.0);
                }
            }
        }
        Some(Self {
            rgba,
            texels,
            // Physical pixels: the callers scale them for their own output.
            origin: (-left, -top),
            size: (f64::from(texels.0), f64::from(texels.1)),
        })
    }
}

/// `value` rounded, saturated to `u32`.
fn u32_from(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a pixel count on a screen, clamped positive"
    )]
    let rounded = value.round().clamp(0.0, f64::from(u32::MAX)) as u32;
    rounded
}

/// `value` rounded, saturated to `i32`.
fn i32_from(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// `value` rounded to whole pixels, saturated.
pub(crate) fn pixels(value: f64) -> i32 {
    i32_from(u32_from(value))
}

/// `value` rounded, saturated to `u8`.
fn u8_from(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a colour channel, clamped to 0..=255 just here"
    )]
    let rounded = value.round().clamp(0.0, 255.0) as u8;
    rounded
}

/// How far `(x, y)` is from the outside of a rounded rect of `width` by
/// `height` whose top left is the origin: negative inside, positive
/// outside.
fn distance(x: f64, y: f64, width: f64, height: f64, radius: f64) -> f64 {
    let dx = (x - width / 2.0).abs() - (width / 2.0 - radius);
    let dy = (y - height / 2.0).abs() - (height / 2.0 - radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    outside + dx.max(dy).min(0.0) - radius
}

/// The complementary error function, to 1e-7 (Abramowitz and Stegun 7.1.26):
/// the coverage of a Gaussian blur past a distance.
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 { poly } else { 2.0 - poly }
}

/// The rows a window's buffer is drawn in so its corners are round: the two
/// corner strips cut to the arc, and the rows between them whole. Front to
/// back along `rect` (output pixels); one row when nothing is cut.
///
/// A row's width is the arc where the row *ends* towards the window's
/// middle, so the buffer never shows outside the corner — the sliver the
/// rounding leaves is what the shadow and the edge show through.
#[must_use]
pub fn bands(rect: Rectangle<i32, Physical>, radius: f64) -> Vec<Rectangle<i32, Physical>> {
    let half = f64::from(rect.size.w.min(rect.size.h)) / 2.0;
    let radius = radius.clamp(0.0, half);
    let rows = i32_from(u32_from(radius.min(radius.floor() + 1.0)));
    if rows < 1 || rect.size.w <= 0 || rect.size.h <= 0 {
        return vec![rect];
    }
    // A corner is at most `MOST_ROWS` rows high, however many pixels it is.
    let step = ((rows + MOST_ROWS - 1) / MOST_ROWS).max(1);
    let expected = usize::try_from(2 * rows / step + 1).unwrap_or(0);
    let mut bands = Vec::with_capacity(expected);
    let mut y = 0;
    while y < rows {
        let next = (y + step).min(rows);
        let inset = inset_at(radius, f64::midpoint(f64::from(y), f64::from(next)));
        bands.push(row(rect, y, next, inset));
        y = next;
    }
    let mut y = rect.size.h - rows;
    while y < rect.size.h {
        let next = (y + step).min(rect.size.h);
        let inset = inset_at(
            radius,
            f64::from(rect.size.h) - f64::midpoint(f64::from(y), f64::from(next)),
        );
        bands.push(row(rect, y, next, inset));
        y = next;
    }
    let middle = rect.size.h - 2 * rows;
    if middle > 0 {
        let mut whole = rect;
        whole.loc.y += rows;
        whole.size.h = middle;
        bands.push(whole);
    }
    bands
}

/// The row of `rect` from `from` to `to` pixels below its top, `inset`
/// pixels in from each side.
fn row(rect: Rectangle<i32, Physical>, from: i32, to: i32, inset: i32) -> Rectangle<i32, Physical> {
    let inset = inset.clamp(0, rect.size.w / 2);
    Rectangle::new(
        (rect.loc.x + inset, rect.loc.y + from).into(),
        ((rect.size.w - 2 * inset).max(0), to - from).into(),
    )
}

/// How far a corner row `depth` pixels from the window's edge is cut in,
/// rounded towards the middle so nothing pokes out of the arc.
fn inset_at(radius: f64, depth: f64) -> i32 {
    let along = (radius - depth).max(0.0);
    let half_chord = (radius * radius - along * along).max(0.0).sqrt();
    i32_from(u32_from(radius - half_chord))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn the_distance_is_negative_inside_and_positive_outside() {
        // A 100 x 50 rect with 10 px corners.
        assert!(distance(50.0, 25.0, 100.0, 50.0, 10.0) < -20.0, "inside");
        assert!(distance(-1.0, 25.0, 100.0, 50.0, 10.0) > 0.0, "left");
        assert!(distance(101.0, 25.0, 100.0, 50.0, 10.0) > 0.0, "right");
        // The corner's arc: the diagonal is outside, the middle of the side
        // is not.
        assert!(distance(0.5, 0.5, 100.0, 50.0, 10.0) > 0.0, "the corner");
        assert!(distance(0.5, 25.0, 100.0, 50.0, 10.0) < 0.0, "the side");
        // Square corners: the corner pixel is inside.
        assert!(distance(0.5, 0.5, 100.0, 50.0, 0.0) < 0.0);
    }

    #[test]
    fn the_blur_covers_what_the_rect_covers() {
        assert!((erfc(-6.0) - 2.0).abs() < 1e-6, "deep inside the shape");
        assert!(erfc(0.0) - 1.0 < 1e-6, "on the outline, half");
        assert!(erfc(6.0) < 1e-6, "far outside");
        assert!(erfc(1.0) > erfc(2.0), "falling");
    }

    #[test]
    fn a_rounded_rect_is_cut_into_rows_that_never_leave_it() {
        let bands = bands(rect(10, 20, 100, 60), 10.0);
        assert_eq!(bands.len(), 21, "ten rows a corner and the middle");
        let first = bands.first().expect("a first row");
        assert_eq!(first.loc.y, 20, "rows start at the window's top");
        assert!(
            first.loc.x > 10 && first.size.w < 90,
            "the top row is cut in on both sides: {first:?}"
        );
        let middle = bands.last().expect("a middle row");
        assert_eq!(middle.size.w, 100, "the middle is whole");
        assert_eq!(middle.size.h, 40);
        for band in &bands {
            assert!(band.size.w > 0 && band.size.h > 0);
            assert!(band.loc.x >= 10 && band.loc.x + band.size.w <= 110);
            assert!(band.loc.y >= 20 && band.loc.y + band.size.h <= 80);
        }
        // Rows from the top and from the bottom together cover every row of
        // the window exactly once.
        let mut rows: Vec<i32> = bands
            .iter()
            .flat_map(|band| (band.loc.y..band.loc.y + band.size.h).collect::<Vec<_>>())
            .collect();
        rows.sort_unstable();
        assert_eq!(rows, (20..80).collect::<Vec<_>>());
    }

    #[test]
    fn a_square_window_is_one_row_and_a_small_one_is_clamped() {
        assert_eq!(bands(rect(0, 0, 40, 40), 0.0), vec![rect(0, 0, 40, 40)]);
        // A radius larger than half the window is a pill: no row is negative.
        let bands = bands(rect(0, 0, 20, 10), 40.0);
        assert!(bands.iter().all(|band| band.size.w > 0));
        let mut rows: Vec<i32> = bands
            .iter()
            .flat_map(|band| (band.loc.y..band.loc.y + band.size.h).collect::<Vec<_>>())
            .collect();
        rows.sort_unstable();
        assert_eq!(rows, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn a_huge_corner_stays_a_few_dozen_rows() {
        let bands = bands(rect(0, 0, 4000, 3000), 500.0);
        assert!(bands.len() <= 2 * MOST_ROWS as usize + 1, "{}", bands.len());
        let mut rows: Vec<i32> = bands
            .iter()
            .flat_map(|band| (band.loc.y..band.loc.y + band.size.h).collect::<Vec<_>>())
            .collect();
        rows.sort_unstable();
        assert_eq!(rows.len(), 3000, "every row is drawn once");
    }

    #[test]
    fn the_default_frame_is_the_window_tile_the_design_language_draws() {
        let frame = WindowFrame::default();
        assert!((frame.radius - 14.0).abs() < f64::EPSILON);
        let shadow = frame.shadow.expect("a shadow");
        assert_eq!(shadow.rgba, [0, 0, 0, 71]);
        assert!((shadow.offset_y - 2.0).abs() < f64::EPSILON);
        assert!((shadow.blur - 16.0).abs() < f64::EPSILON);
        let edge = frame.edge.expect("an edge");
        assert_eq!(edge.rgba, [255, 255, 255, 36]);
        assert!((edge.width - 1.0).abs() < f64::EPSILON);
        assert!(!frame.is_square());
        assert!(WindowFrame::square().is_square());
    }

    #[test]
    fn the_shadow_falls_below_the_window_and_the_edge_runs_round_it() {
        let frame = WindowFrame {
            radius: 8.0,
            shadow: Some(Shadow {
                offset_y: 0.0,
                blur: 4.0,
                rgba: [0, 0, 0, 255],
            }),
            edge: Some(Edge {
                width: 1.0,
                rgba: [255, 255, 255, 255],
            }),
        };
        let size = (100.0, 60.0);
        let signature = Signature {
            size: (6400, 3840),
            radius: sixtyfourths(frame.radius),
            shadow: Some((0, sixtyfourths(4.0), [0, 0, 0, 255])),
            edge: Some((sixtyfourths(1.0), [255, 255, 255, 255])),
        };
        let image = Raster::of(&signature).expect("an image");
        let (width, height) = image.texels;
        let pixel = |x: u32, y: u32| -> [u8; 4] {
            let at = ((y * width + x) * 4) as usize;
            let slice = image.rgba.get(at..at + 4).expect("a pixel");
            [slice[0], slice[1], slice[2], slice[3]]
        };
        // The image starts above and left of the window by its blur's reach.
        assert!(image.origin.0 < 0.0 && image.origin.1 < 0.0);
        assert!(image.size.0 > size.0 && image.size.1 > size.1);
        let left = u32_from(-image.origin.0);
        let top = u32_from(-image.origin.1);
        let middle = pixel(left + 50, top + 30);
        assert_eq!(middle[3], 0, "the window's own pixels are not drawn");
        let above = pixel(left + 50, top - 1);
        assert!(above[3] > 0, "the edge above the window: {above:?}");
        assert!(above[0] > 0, "and it is the light one");
        let below = pixel(left + 50, top + 60 + 3);
        assert!(below[3] > 0 && below[0] == 0, "the shadow below: {below:?}");
        assert!(
            left + 50 < width && top + 60 + 1 < height,
            "the image reaches below the window"
        );
    }

    #[test]
    fn an_image_is_drawn_once_per_size_and_frame() {
        let images = FrameImages::default();
        let frame = WindowFrame::default();
        let first = images.image(&frame, (200.0, 100.0), 1.0).expect("one");
        let again = images.image(&frame, (200.0, 100.0), 1.0).expect("the same");
        assert!(std::rc::Rc::ptr_eq(&first, &again));
        assert!(
            images.image(&frame, (200.0, 100.0), 2.0).is_some(),
            "another output scale is another image"
        );
        assert!(
            images
                .image(&WindowFrame::square(), (200.0, 100.0), 1.0)
                .is_none()
        );
        assert!(images.image(&frame, (0.0, 100.0), 1.0).is_none());
    }
}
