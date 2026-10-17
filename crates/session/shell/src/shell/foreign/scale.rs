//! How a surface's buffer lies on its surface-local coordinates, and how a
//! toplevel and its subsurfaces become one image.
//!
//! Wayland places everything — subsurface offsets, window geometry, pointer
//! positions — in surface-local *logical* pixels. A buffer holds its surface
//! at a density of its own: `wl_surface.set_buffer_scale`, or whatever ratio a
//! `wp_viewport` sets between the buffer (or its source rectangle) and the
//! destination size. Surfaces of one window need not agree: a browser renders
//! its page into a subsurface at a scale different from its toplevel's, or
//! gives the toplevel a single stretched pixel. So a tile's image is a canvas
//! at the densest surface's density, each surface resampled onto it at its
//! logical place.

use super::{ChildPixels, FrameSnapshot, WindowGeometry};

/// What a commit said about how a surface's buffer maps onto the surface:
/// `wl_surface.set_buffer_scale` and the `wp_viewport` rectangles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceScale {
    /// `wl_surface.set_buffer_scale`.
    pub buffer_scale: i32,
    /// `wp_viewport.set_source`: `(x, y, width, height)` of the buffer, in
    /// buffer pixels divided by the buffer scale.
    pub source: Option<(f64, f64, f64, f64)>,
    /// `wp_viewport.set_destination`: the surface's logical size.
    pub destination: Option<(i32, i32)>,
}

impl Default for SurfaceScale {
    fn default() -> Self {
        Self {
            buffer_scale: 1,
            source: None,
            destination: None,
        }
    }
}

/// A rectangle in some pixel grid, fractional where a mapping makes it so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PixelRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PixelRect {
    /// All of a `width`×`height` grid.
    pub fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: f64::from(width),
            height: f64::from(height),
        }
    }
}

/// A buffer laid on its surface: the part of it that is shown and the
/// logical extent that part covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BufferPlacement {
    /// Buffer pixels.
    pub source: PixelRect,
    /// Logical pixels.
    pub logical: (f64, f64),
}

impl BufferPlacement {
    /// Buffer pixels per logical pixel, along each axis.
    pub fn density(&self) -> (f64, f64) {
        (
            self.source.width / self.logical.0,
            self.source.height / self.logical.1,
        )
    }
}

impl SurfaceScale {
    /// How a `width`×`height` buffer lies on the surface; `None` for an empty
    /// buffer. A viewport rectangle the protocol would reject (empty, or
    /// reaching outside the buffer) is ignored rather than trusted.
    pub(super) fn place(&self, width: u32, height: u32) -> Option<BufferPlacement> {
        if width == 0 || height == 0 {
            return None;
        }
        let scale = f64::from(self.buffer_scale.max(1));
        let whole = PixelRect::whole(width, height);
        let source = self.source.filter(|&(x, y, w, h)| {
            x >= 0.0
                && y >= 0.0
                && w > 0.0
                && h > 0.0
                && (x + w) * scale <= whole.width
                && (y + h) * scale <= whole.height
        });
        let logical = match (self.destination, source) {
            (Some((w, h)), _) if w > 0 && h > 0 => (f64::from(w), f64::from(h)),
            (_, Some((_, _, w, h))) => (w, h),
            _ => (whole.width / scale, whole.height / scale),
        };
        let source = source.map_or(whole, |(x, y, w, h)| PixelRect {
            x: x * scale,
            y: y * scale,
            width: w * scale,
            height: h * scale,
        });
        Some(BufferPlacement { source, logical })
    }

    /// Buffer pixels per logical pixel for a `width`×`height` buffer.
    pub fn density(&self, width: u32, height: u32) -> Option<(f64, f64)> {
        self.place(width, height).map(|placed| placed.density())
    }
}

/// The image a toplevel and its subsurfaces are flattened into: the
/// toplevel's logical extent at the density of its densest surface.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "`root_is_canvas` reads as the claim it makes about the toplevel buffer"
)]
pub(super) struct Canvas {
    /// Canvas pixels per logical pixel.
    pub density: (f64, f64),
    /// The whole toplevel surface, canvas pixels.
    pub size: (u32, u32),
    /// The toplevel's buffer is the canvas as it stands: all of it is shown
    /// and nothing needs resampling.
    pub root_is_canvas: bool,
}

impl Canvas {
    /// The canvas for a `root_size` toplevel buffer placed as `root`, with
    /// subsurfaces placed as `children`.
    pub fn new(
        root: &BufferPlacement,
        root_size: (u32, u32),
        children: impl IntoIterator<Item = BufferPlacement>,
    ) -> Self {
        let (mut dx, mut dy) = root.density();
        for child in children {
            let (cx, cy) = child.density();
            dx = dx.max(cx);
            dy = dy.max(cy);
        }
        // Raising a window to its densest subsurface stops at the client
        // texture limit, unless the toplevel's own buffer is bigger still.
        let limit = f64::from(super::CLIENT_TEXTURE_LIMIT_PX)
            .max(f64::from(root_size.0))
            .max(f64::from(root_size.1));
        let fit = (limit / (root.logical.0 * dx))
            .min(limit / (root.logical.1 * dy))
            .min(1.0);
        let density = (dx * fit, dy * fit);
        let size = (
            canvas_extent(root.logical.0 * density.0),
            canvas_extent(root.logical.1 * density.1),
        );
        let root_is_canvas =
            root.source == PixelRect::whole(root_size.0, root_size.1) && size == root_size;
        Self {
            density,
            size,
            root_is_canvas,
        }
    }

    /// The window inside the canvas, `(x, y, width, height)` in canvas
    /// pixels: the declared geometry when there is one, otherwise all of it.
    pub fn window(&self, geometry: Option<WindowGeometry>) -> (u32, u32, u32, u32) {
        geometry
            .and_then(|geometry| geometry.crop(self.size.0, self.size.1, self.density))
            .unwrap_or((0, 0, self.size.0, self.size.1))
    }

    /// Where a surface `placed` at logical `(x, y)` on the toplevel lands.
    pub fn destination(&self, (x, y): (i32, i32), placed: &BufferPlacement) -> PixelRect {
        PixelRect {
            x: f64::from(x) * self.density.0,
            y: f64::from(y) * self.density.1,
            width: placed.logical.0 * self.density.0,
            height: placed.logical.1 * self.density.1,
        }
    }
}

/// A canvas side in whole pixels, at least one.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped into [1, u32::MAX] first, so the cast neither truncates a \
              fraction nor loses a sign"
)]
fn canvas_extent(value: f64) -> u32 {
    if value.is_nan() {
        return 1;
    }
    value.round().clamp(1.0, f64::from(u32::MAX)) as u32
}

/// A toplevel's image and how it relates to the surface: what [`Canvas`]
/// makes of it and the window cut out of that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FrameLayout {
    pub canvas: Canvas,
    /// The window, `(x, y, width, height)` in canvas pixels; the image is
    /// exactly this.
    pub content: (u32, u32, u32, u32),
}

impl FrameLayout {
    /// The image pixel showing surface-local `(x, y)`.
    pub fn to_image(self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x * self.canvas.density.0 - f64::from(self.content.0),
            y * self.canvas.density.1 - f64::from(self.content.1),
        )
    }

    /// The surface-local point image pixel `(x, y)` shows.
    pub fn to_surface(self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            (x + f64::from(self.content.0)) / self.canvas.density.0,
            (y + f64::from(self.content.1)) / self.canvas.density.1,
        )
    }
}

/// How a resampled layer combines with what is under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Blend {
    /// Take the layer's pixels: the toplevel onto an empty canvas.
    Replace,
    /// Over what is there: a subsurface onto its parent.
    Over,
}

/// Draw the `source` part of `layer` (buffer pixels) into `destination`
/// (canvas pixels), bilinearly filtered. A pixel whose centre lies inside
/// `destination` is covered; sampling never reaches outside `source`, so a
/// crop does not bleed in its neighbours.
pub(super) fn draw_resampled(
    canvas: &mut FrameSnapshot,
    layer: &ChildPixels<'_>,
    source: PixelRect,
    destination: PixelRect,
    blend: Blend,
) {
    let finite = [
        source.x,
        source.y,
        source.width,
        source.height,
        destination.x,
        destination.y,
        destination.width,
        destination.height,
    ]
    .iter()
    .all(|value| value.is_finite());
    if !finite
        || layer.width == 0
        || layer.height == 0
        || destination.width <= 0.0
        || destination.height <= 0.0
        || source.width <= 0.0
        || source.height <= 0.0
    {
        return;
    }
    let columns = taps(
        canvas.width,
        (destination.x, destination.width),
        (source.x, source.width),
        layer.width,
    );
    let rows = taps(
        canvas.height,
        (destination.y, destination.height),
        (source.y, source.height),
        layer.height,
    );
    let stride = usize::try_from(layer.width).unwrap_or(usize::MAX);
    let canvas_stride = usize::try_from(canvas.width).unwrap_or(usize::MAX);
    let texel = |y: usize, x: usize| {
        let start = y.saturating_mul(stride).saturating_add(x).saturating_mul(4);
        layer
            .rgba
            .get(start..start.saturating_add(4))
            .and_then(|pixel| <[u8; 4]>::try_from(pixel).ok())
            .unwrap_or([0; 4])
    };
    for row in &rows {
        for column in &columns {
            let (top_left, top_right, bottom_left, bottom_right) = (
                texel(row.near, column.near),
                texel(row.near, column.far),
                texel(row.far, column.near),
                texel(row.far, column.far),
            );
            let mut sample = [0_u8; 4];
            for (channel, out) in sample.iter_mut().enumerate() {
                let at = |pixel: [u8; 4]| f32::from(pixel.get(channel).copied().unwrap_or(0));
                let top = at(top_left) + (at(top_right) - at(top_left)) * column.weight;
                let bottom = at(bottom_left) + (at(bottom_right) - at(bottom_left)) * column.weight;
                *out = channel_u8(top + (bottom - top) * row.weight);
            }
            let start = row
                .target
                .saturating_mul(canvas_stride)
                .saturating_add(column.target)
                .saturating_mul(4);
            let Some(Ok(pixel)) = canvas
                .rgba
                .get_mut(start..start.saturating_add(4))
                .map(<&mut [u8; 4]>::try_from)
            else {
                continue;
            };
            match blend {
                Blend::Replace => *pixel = sample,
                Blend::Over => super::blend_over(pixel, sample),
            }
        }
    }
}

/// One canvas row or column a layer covers, and the two layer texels
/// (`near`, `far`, mixed by `weight` towards `far`) it samples.
struct Tap {
    target: usize,
    near: usize,
    far: usize,
    weight: f32,
}

/// The [`Tap`]s along one axis: canvas pixels `0..extent` whose centres fall
/// in `(start, length)`, sampling `(source_start, source_length)` of a layer
/// `limit` texels long.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "every cast value is floored or ceiled and clamped into [0, extent] or \
              [0, limit - 1] first, both u32 ranges, so none truncates a fraction, loses a \
              sign or wraps"
)]
fn taps(
    extent: u32,
    (start, length): (f64, f64),
    (source_start, source_length): (f64, f64),
    limit: u32,
) -> Vec<Tap> {
    let last = f64::from(limit.saturating_sub(1));
    // Texels the source rectangle covers, so filtering stays inside it.
    let low = source_start.floor().clamp(0.0, last);
    let high = ((source_start + source_length).ceil() - 1.0).clamp(low, last);
    let ratio = source_length / length;
    let first = start.floor().clamp(0.0, f64::from(extent)) as u32;
    let end = (start + length).ceil().clamp(0.0, f64::from(extent)) as u32;
    (first..end)
        .filter_map(|target| {
            let centre = f64::from(target) + 0.5;
            if centre < start || centre >= start + length {
                return None;
            }
            let at = (source_start + (centre - start) * ratio - 0.5).clamp(low, high);
            let near = at.floor();
            let far = (near + 1.0).min(high);
            Some(Tap {
                target: target as usize,
                near: near as usize,
                far: far as usize,
                weight: (at - near) as f32,
            })
        })
        .collect()
}

/// A filtered channel value back in a byte.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped into [0, 255] first"
)]
fn channel_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_scale_or_viewport_sets_the_density() {
        let integer = SurfaceScale {
            buffer_scale: 2,
            ..SurfaceScale::default()
        };
        assert_eq!(integer.density(240, 120), Some((2.0, 2.0)));
        let placed = integer.place(240, 120).expect("placed");
        assert_eq!(placed.logical, (120.0, 60.0));

        // Fractional scaling: a 1.5x buffer declared at its logical size.
        let fractional = SurfaceScale {
            destination: Some((200, 100)),
            ..SurfaceScale::default()
        };
        assert_eq!(fractional.density(300, 150), Some((1.5, 1.5)));

        // A single pixel stretched over the whole window.
        let stretched = SurfaceScale {
            destination: Some((800, 600)),
            ..SurfaceScale::default()
        };
        assert_eq!(stretched.density(1, 1), Some((1.0 / 800.0, 1.0 / 600.0)));

        // A source crop without a destination shows the crop at its own size.
        let cropped = SurfaceScale {
            buffer_scale: 2,
            source: Some((10.0, 5.0, 50.0, 25.0)),
            destination: None,
        };
        let placed = cropped.place(240, 120).expect("placed");
        assert_eq!(
            placed.source,
            PixelRect {
                x: 20.0,
                y: 10.0,
                width: 100.0,
                height: 50.0
            }
        );
        assert_eq!(placed.logical, (50.0, 25.0));

        let outside = SurfaceScale {
            source: Some((0.0, 0.0, 500.0, 500.0)),
            ..SurfaceScale::default()
        };
        assert_eq!(
            outside.place(100, 100).map(|placed| placed.source),
            Some(PixelRect::whole(100, 100)),
            "a source the protocol would reject is not trusted"
        );
        assert_eq!(SurfaceScale::default().place(0, 10), None);
    }

    #[test]
    fn the_canvas_takes_the_densest_surface() {
        let root = SurfaceScale::default().place(400, 300).expect("root");
        let page = SurfaceScale {
            buffer_scale: 2,
            ..SurfaceScale::default()
        }
        .place(400, 300)
        .expect("page");
        let canvas = Canvas::new(&root, (400, 300), [page]);
        assert_eq!(canvas.density, (2.0, 2.0));
        assert_eq!(canvas.size, (800, 600));
        assert!(!canvas.root_is_canvas, "the 1x toplevel is raised to 2x");

        let alone = Canvas::new(&root, (400, 300), []);
        assert_eq!(alone.size, (400, 300));
        assert!(alone.root_is_canvas);

        let huge = SurfaceScale {
            destination: Some((10, 10)),
            ..SurfaceScale::default()
        }
        .place(4000, 4000)
        .expect("dense child");
        let bounded = Canvas::new(&root, (400, 300), [huge]);
        assert!(
            bounded.size.0 <= super::super::CLIENT_TEXTURE_LIMIT_PX.unsigned_abs(),
            "an absurdly dense subsurface cannot make an unbounded canvas"
        );
    }

    #[test]
    fn resampling_at_one_to_one_copies_exactly_and_upscaling_fills_the_rect() {
        let pixels: Vec<u8> = (0..4_u8).flat_map(|v| [v * 60, 0, 0, 255]).collect();
        let layer = ChildPixels {
            rgba: &pixels,
            width: 2,
            height: 2,
        };
        let mut canvas = FrameSnapshot {
            rgba: vec![0; 4 * 4 * 4],
            width: 4,
            height: 4,
            seq: 0,
            hdr: None,
        };
        draw_resampled(
            &mut canvas,
            &layer,
            PixelRect::whole(2, 2),
            PixelRect {
                x: 1.0,
                y: 1.0,
                width: 2.0,
                height: 2.0,
            },
            Blend::Replace,
        );
        let red = |canvas: &FrameSnapshot, x: usize, y: usize| canvas.rgba[(y * 4 + x) * 4];
        let alpha = |canvas: &FrameSnapshot, x: usize, y: usize| canvas.rgba[(y * 4 + x) * 4 + 3];
        assert_eq!(
            [
                red(&canvas, 1, 1),
                red(&canvas, 2, 1),
                red(&canvas, 1, 2),
                red(&canvas, 2, 2)
            ],
            [0, 60, 120, 180],
            "aligned 1:1 is a copy"
        );
        assert_eq!(alpha(&canvas, 0, 0), 0, "outside the destination untouched");
        assert_eq!(alpha(&canvas, 3, 3), 0);

        canvas.rgba.fill(0);
        draw_resampled(
            &mut canvas,
            &layer,
            PixelRect::whole(2, 2),
            PixelRect::whole(4, 4),
            Blend::Over,
        );
        assert!(
            canvas.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255),
            "a 2x2 layer drawn over 4x4 covers every pixel"
        );
        assert_eq!(red(&canvas, 0, 0), 0, "corners keep their texel");
        assert_eq!(red(&canvas, 3, 3), 180);
    }
}
