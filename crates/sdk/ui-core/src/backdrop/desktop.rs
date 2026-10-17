//! The desktop, blurred, for surfaces that stand on it alone.
//!
//! A window group's docked tab strip is frosted over the desktop, and only
//! the desktop is ever behind it: windows never overlap. So its blur needs
//! no pass over what the frame drew before it ([`super::BackdropBlur`]),
//! only the desktop, which changes when the wallpaper, the displays or the
//! theme's blur do. The desktop's painter publishes it blurred once per
//! such change ([`publish`], [`blurred_cover`]); a strip fills its body
//! with that picture and the material's tint ([`fill`]). The strip is then
//! an ordinary image fill: it encodes the same while nothing behind it
//! changed, a hover on a tab damages only the tab, and a presenter that
//! moves a strip's retained pixels during a pan moves its frosting with
//! it, to be drawn again where it comes to rest.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "image resampling: sizes and indices are bounded by the image and clamped before \
              every cast"
)]

use std::cell::RefCell;

use vello::Scene;
use vello::kurbo::{Affine, Rect, Shape};
use vello::peniko::{BlendMode, Brush, Color, Extend, Fill, ImageBrush, ImageData, ImageFormat};

use crate::floating_surface::Material;
use crate::theme::Theme;

/// One display's desktop, blurred: `brush` placed by `brush_transform` over
/// `rect`, all in the coordinates `transform` maps into the desktop's.
#[derive(Clone, Debug)]
pub struct DesktopPiece {
    pub transform: Affine,
    pub rect: Rect,
    pub brush: Brush,
    pub brush_transform: Affine,
}

/// The longest edge of a blurred desktop, in texels. Blurred, the desktop
/// is a low-frequency picture; a few hundred texels across hold all of it.
const MAX_EDGE: u32 = 512;

/// A blur reaches three standard deviations.
const REACH: f64 = 3.0;

/// Blurred desktops already made, by wallpaper, size and radius, so a
/// frame that draws the same desktop costs nothing.
type CoverKey = (u64, u64, u64, u32);

thread_local! {
    static DESKTOP: RefCell<Vec<DesktopPiece>> = const { RefCell::new(Vec::new()) };
    static COVERS: RefCell<Vec<(CoverKey, ImageData)>> = const { RefCell::new(Vec::new()) };
}

/// The desktop a frame draws, blurred, in the coordinates of the scene the
/// desktop is drawn into. The desktop's painter publishes it every time it
/// paints; an empty list says there is no picture to frost with, and
/// surfaces over the desktop are then opaque.
pub fn publish(pieces: Vec<DesktopPiece>) {
    DESKTOP.with(|slot| *slot.borrow_mut() = pieces);
}

/// Whether a desktop is published on this thread.
#[must_use]
pub fn published() -> bool {
    DESKTOP.with(|slot| !slot.borrow().is_empty())
}

/// Fill `shape` with `material` over the published desktop: the blurred
/// desktop behind it, under the material's tint of `ground`.
/// `desktop_to_scene` maps the desktop's coordinates into the scene's (a
/// strip drawn in canvas coordinates is drawn moved by its pan). Opaque
/// `ground` where the material does not blur or nothing is published.
pub(crate) fn fill(
    scene: &mut Scene,
    (transform, desktop_to_scene): (Affine, Affine),
    shape: &impl Shape,
    material: Material,
    theme: &Theme,
    ground: Color,
) {
    let pieces = DESKTOP.with(|slot| slot.borrow().clone());
    if !super::blurs(material, theme) || pieces.is_empty() {
        scene.fill(Fill::NonZero, transform, ground, None, shape);
        return;
    }
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, shape);
    // The first display's picture, its edges carried on, fills all of the
    // shape (a strip panned off a display still frosts); every other
    // display's covers its own rect.
    for (index, piece) in pieces.iter().enumerate() {
        let to_scene = desktop_to_scene * piece.transform;
        if index == 0 {
            scene.fill(
                Fill::NonZero,
                transform,
                &piece.brush,
                Some(transform.inverse() * to_scene * piece.brush_transform),
                shape,
            );
        } else {
            scene.fill(
                Fill::NonZero,
                to_scene,
                &piece.brush,
                Some(piece.brush_transform),
                &piece.rect,
            );
        }
    }
    scene.pop_layer();
    scene.fill(
        Fill::NonZero,
        transform,
        ground.with_alpha(theme.tokens.colors.surfaces.material_opacity),
        None,
        shape,
    );
}

/// `image` covering `rect` as a desktop paints a wallpaper (scaled to cover
/// it, centred), blurred by `blur_px` logical pixels: the blurred picture
/// and the transform that places it over `rect`. Made once per wallpaper,
/// size and radius on this thread. `None` for an image whose pixels are not
/// in memory.
#[must_use]
pub fn blurred_cover(image: &ImageData, rect: Rect, blur_px: f32) -> Option<(ImageData, Affine)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || image.width == 0 || image.height == 0 {
        return None;
    }
    let blur = f64::from(blur_px.max(0.0));
    // A texel small enough that the blur spans a few of them.
    let texel = (blur / 3.0)
        .max(rect.width().max(rect.height()) / f64::from(MAX_EDGE))
        .max(1.0);
    let width = (rect.width() / texel).ceil().max(1.0) as u32;
    let height = (rect.height() / texel).ceil().max(1.0) as u32;
    let key = (
        image.data.id(),
        rect.width().to_bits(),
        rect.height().to_bits(),
        blur_px.to_bits(),
    );
    let placed = Affine::translate((rect.x0, rect.y0)).pre_scale_non_uniform(
        rect.width() / f64::from(width),
        rect.height() / f64::from(height),
    );
    if let Some(found) = COVERS.with(|covers| {
        covers
            .borrow()
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, image)| image.clone())
    }) {
        return Some((found, placed));
    }
    let pixels = image.data.data();
    let stride = image.width as usize * 4;
    if pixels.len() < stride * image.height as usize {
        return None;
    }
    let mut out = cover_sample(image, pixels, (rect, width, height));
    blur_rows(&mut out, (width, height), blur / texel);
    let blurred = crate::raster::rgba8(to_bytes(&out), width, height);
    COVERS.with(|covers| {
        let mut covers = covers.borrow_mut();
        // Only the desktops on screen are worth keeping.
        if covers.len() >= 8 {
            covers.remove(0);
        }
        covers.push((key, blurred.clone()));
    });
    Some((blurred, placed))
}

/// The desktop's pieces with no wallpaper: `brush` (a gradient, already as
/// smooth as its blur) over `rect`.
#[must_use]
pub fn smooth_piece(transform: Affine, rect: Rect, brush: Brush) -> DesktopPiece {
    DesktopPiece {
        transform,
        rect,
        brush,
        brush_transform: Affine::IDENTITY,
    }
}

/// A blurred wallpaper's piece, from [`blurred_cover`].
#[must_use]
pub fn image_piece(
    transform: Affine,
    rect: Rect,
    (image, placed): (ImageData, Affine),
) -> DesktopPiece {
    DesktopPiece {
        transform,
        rect,
        brush: Brush::Image(ImageBrush::new(image).with_extend(Extend::Pad)),
        brush_transform: placed,
    }
}

/// Average the wallpaper, covering `rect`, onto a `width`×`height` grid:
/// each texel the mean of a few samples across the wallpaper pixels it
/// covers. Linear-light averaging is not needed under a blur this wide.
fn cover_sample(
    image: &ImageData,
    pixels: &[u8],
    (rect, width, height): (Rect, u32, u32),
) -> Vec<[f32; 4]> {
    let (iw, ih) = (f64::from(image.width), f64::from(image.height));
    let scale = (rect.width() / iw).max(rect.height() / ih);
    let off_x = (rect.width() - iw * scale) / 2.0;
    let off_y = (rect.height() - ih * scale) / 2.0;
    let tx = rect.width() / f64::from(width);
    let ty = rect.height() / f64::from(height);
    // Samples per texel side: two are plenty under a blur this wide, and
    // keep the one-off cost of a 4K wallpaper low.
    let per = ((tx / scale).ceil() as usize).clamp(1, 2);
    let bgra = image.format == ImageFormat::Bgra8;
    let stride = image.width as usize * 4;
    let mut out = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0.0_f32; 4];
            for sy in 0..per {
                for sx in 0..per {
                    let px = (f64::from(x) + (sx as f64 + 0.5) / per as f64) * tx;
                    let py = (f64::from(y) + (sy as f64 + 0.5) / per as f64) * ty;
                    let ix = ((px - off_x) / scale).clamp(0.0, iw - 1.0) as usize;
                    let iy = ((py - off_y) / scale).clamp(0.0, ih - 1.0) as usize;
                    let at = iy * stride + ix * 4;
                    let [c0, c1, c2, c3] = pixels
                        .get(at..at + 4)
                        .and_then(|texel| <[u8; 4]>::try_from(texel).ok())
                        .unwrap_or([0, 0, 0, 255]);
                    let (r, b) = if bgra { (c2, c0) } else { (c0, c2) };
                    for (slot, value) in sum.iter_mut().zip([r, c1, b, c3]) {
                        *slot += f32::from(value);
                    }
                }
            }
            let n = (per * per) as f32;
            out.push(sum.map(|channel| channel / n));
        }
    }
    out
}

/// A separable Gaussian of `sigma` texels over `texels`, edges clamped.
fn blur_rows(texels: &mut [[f32; 4]], (width, height): (u32, u32), sigma: f64) {
    if sigma < 0.3 {
        return;
    }
    let radius = (sigma * REACH).ceil() as i64;
    let weights: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32)
        .collect();
    let total: f32 = weights.iter().sum();
    let (w, h) = (i64::from(width), i64::from(height));
    let pass = |src: &[[f32; 4]], along_x: bool| -> Vec<[f32; 4]> {
        let mut dst = vec![[0.0_f32; 4]; src.len()];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0_f32; 4];
                for (k, weight) in weights.iter().enumerate() {
                    let d = k as i64 - radius;
                    let (sx, sy) = if along_x {
                        ((x + d).clamp(0, w - 1), y)
                    } else {
                        (x, (y + d).clamp(0, h - 1))
                    };
                    let texel = src.get((sy * w + sx) as usize).copied().unwrap_or_default();
                    for (slot, value) in acc.iter_mut().zip(texel) {
                        *slot += value * weight;
                    }
                }
                if let Some(slot) = dst.get_mut((y * w + x) as usize) {
                    *slot = acc.map(|channel| channel / total);
                }
            }
        }
        dst
    };
    let across = pass(texels, true);
    let down = pass(&across, false);
    texels.copy_from_slice(&down);
}

fn to_bytes(texels: &[[f32; 4]]) -> Vec<u8> {
    texels
        .iter()
        .flat_map(|texel| texel.map(|channel| channel.round().clamp(0.0, 255.0) as u8))
        .collect()
}
