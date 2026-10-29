//! The pointer's image: the surface a client set, or a named shape from
//! the cursor theme.
//!
//! Backends that own the screen draw it in front of everything else (the
//! DRM backend's `DrmCompositor` puts it on the cursor plane where there is
//! one). Named shapes come from the cursor theme `XCURSOR_THEME` names
//! (`default` otherwise) at `XCURSOR_SIZE` (24) times the output's scale;
//! without a theme a plain arrow is drawn.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical cursor positions, far from i32 limits"
)]

use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::output::Output;
use smithay::utils::{Physical, Point, Scale, Transform};

use crate::render::OutputRenderElement;
use crate::state::State;

/// The cursor size, in logical pixels, when `XCURSOR_SIZE` does not say.
const DEFAULT_SIZE: u32 = 24;

/// One image of a named shape at one size.
struct Image {
    buffer: MemoryRenderBuffer,
    /// The hotspot, in the image's pixels.
    hotspot: Point<i32, Physical>,
}

/// What the pointer shows and the theme images loaded so far.
pub(crate) struct Cursor {
    pub(crate) status: CursorImageStatus,
    theme: xcursor::CursorTheme,
    size: u32,
    images: HashMap<(CursorIcon, u32), Image>,
}

impl Cursor {
    pub(crate) fn new() -> Self {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|size| size.parse().ok())
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_SIZE);
        Self {
            status: CursorImageStatus::default_named(),
            theme: xcursor::CursorTheme::load(&theme),
            size,
            images: HashMap::new(),
        }
    }

    /// The image of `icon` at `pixels` physical size, loaded on first use.
    fn image(&mut self, icon: CursorIcon, pixels: u32) -> &Image {
        let theme = &self.theme;
        self.images
            .entry((icon, pixels))
            .or_insert_with(|| themed(theme, icon, pixels).unwrap_or_else(|| arrow(pixels)))
    }
}

/// `icon` from the theme, under its name or an older alias, at the size
/// nearest `pixels`.
fn themed(theme: &xcursor::CursorTheme, icon: CursorIcon, pixels: u32) -> Option<Image> {
    let path = std::iter::once(icon.name())
        .chain(icon.alt_names().iter().copied())
        .find_map(|name| theme.load_icon(name))?;
    let bytes = std::fs::read(path).ok()?;
    let images = xcursor::parser::parse_xcursor(&bytes)?;
    let image = images
        .iter()
        .min_by_key(|image| image.size.abs_diff(pixels))?;
    let size = (
        i32::try_from(image.width).ok()?,
        i32::try_from(image.height).ok()?,
    );
    Some(Image {
        // RGBA bytes are ABGR8888 in DRM's little-endian naming.
        buffer: MemoryRenderBuffer::from_slice(
            &image.pixels_rgba,
            Fourcc::Abgr8888,
            size,
            1,
            Transform::Normal,
            None,
        ),
        hotspot: Point::from((
            i32::try_from(image.xhot).ok()?,
            i32::try_from(image.yhot).ok()?,
        )),
    })
}

/// A plain arrow, white with a black edge, `pixels` tall, for systems with
/// no cursor theme.
fn arrow(pixels: u32) -> Image {
    let side = pixels.max(8);
    let rgba = arrow_pixels(side);
    let extent = i32::try_from(side).unwrap_or(24);
    Image {
        buffer: MemoryRenderBuffer::from_slice(
            &rgba,
            Fourcc::Abgr8888,
            (extent, extent),
            1,
            Transform::Normal,
            None,
        ),
        hotspot: Point::from((0, 0)),
    }
}

/// RGBA rows of a `side`-pixel arrow: a right triangle with its point at
/// the top left, as wide as it is tall at the bottom row's half.
fn arrow_pixels(side: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        // Each row is half a pixel wider than the last.
        let width = u32::midpoint(y, 2);
        for x in 0..side {
            let pixel: [u8; 4] = if x > width {
                [0, 0, 0, 0]
            } else if x == 0 || x == width || y + 1 == side {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

impl State {
    /// The pointer's elements on `output`, front first, in its physical
    /// pixels; none when the pointer is elsewhere or hidden.
    pub(crate) fn cursor_elements<R>(
        &mut self,
        renderer: &mut R,
        output: &Output,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let Some(geometry) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        if !geometry.to_f64().contains(self.pointer) {
            return Vec::new();
        }
        let scale = output.current_scale().fractional_scale();
        let at: Point<i32, Physical> =
            (self.pointer - geometry.loc.to_f64()).to_physical_precise_round(scale);
        // A hosted surface under the pointer shows the default shape.
        let status = if self.pointer_over_host() {
            CursorImageStatus::default_named()
        } else {
            self.cursor.status.clone()
        };
        match status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = smithay::wayland::compositor::with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .and_then(|data| data.lock().ok().map(|data| data.hotspot))
                        .unwrap_or_default()
                });
                let origin = at - hotspot.to_physical_precise_round(scale);
                render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    origin,
                    Scale::from(scale),
                    1.0,
                    Kind::Cursor,
                )
            }
            CursorImageStatus::Named(icon) => {
                let pixels = cursor_pixels(self.cursor.size, scale);
                let image = self.cursor.image(icon, pixels);
                let origin = at - image.hotspot;
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    origin.to_f64(),
                    &image.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => vec![OutputRenderElement::Hosted(element)],
                    Err(error) => {
                        tracing::warn!(?error, "cursor image not imported");
                        Vec::new()
                    }
                }
            }
        }
    }
}

/// The physical size of a `size`-logical-pixel cursor at `scale`.
fn cursor_pixels(size: u32, scale: f64) -> u32 {
    let pixels = (f64::from(size) * scale).round();
    if pixels.is_finite() && pixels >= 1.0 && pixels <= f64::from(u16::MAX) {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked to lie in 1..=u16::MAX above"
        )]
        let pixels = pixels as u32;
        pixels
    } else {
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_grow_with_the_output_scale() {
        assert_eq!(cursor_pixels(24, 1.0), 24);
        assert_eq!(cursor_pixels(24, 1.5), 36);
        assert_eq!(cursor_pixels(24, 2.0), 48);
        assert_eq!(cursor_pixels(24, f64::NAN), 24);
        assert_eq!(cursor_pixels(24, 0.0), 24);
    }

    #[test]
    fn the_fallback_arrow_points_at_the_top_left() {
        let side = 16;
        let rgba = arrow_pixels(side);
        assert_eq!(rgba.len(), 16 * 16 * 4);
        let pixel = |x: usize, y: usize| {
            let at = (y * 16 + x) * 4;
            rgba.get(at..at + 4).map(<[u8]>::to_vec)
        };
        // The point is opaque, the far top right is clear, the middle of
        // the bottom-left half is white.
        assert_eq!(pixel(0, 0), Some(vec![0, 0, 0, 255]));
        assert_eq!(pixel(15, 0), Some(vec![0, 0, 0, 0]));
        assert_eq!(pixel(2, 10), Some(vec![255, 255, 255, 255]));
    }
}
