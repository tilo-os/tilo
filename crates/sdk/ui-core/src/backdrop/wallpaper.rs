//! The shell, blurred, for surfaces that stand on it alone. A window group's docked tab strip is
//! frosted over the shell, and only the shell is ever behind it: windows never overlap.
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
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Shape};
use vello::peniko::{
    BlendMode, Brush, Color, Extend, Fill, Gradient, ImageBrush, ImageData, ImageFormat,
};

use crate::floating_surface::Material;
use crate::theme::Theme;

const MAX_EDGE: u32 = 512;

const REACH: f64 = 3.0;

/// What a blurred cover is made for: the picture, the rectangle it covers and the blur.
type CoverKey = (u64, [u64; 4], u32);

fn cover_key(picture: &ImageData, rect: Rect, blur_px: f32) -> CoverKey {
    let sides = [
        rect.x0.to_bits(),
        rect.y0.to_bits(),
        rect.width().to_bits(),
        rect.height().to_bits(),
    ];
    (picture.data.id(), sides, blur_px.to_bits())
}

thread_local! {
    static COVERS: RefCell<Vec<(CoverKey, ImageData)>> = const { RefCell::new(Vec::new()) };
}

/// The shell's wallpaper: one source, the picture the session decoded once for all its hosts.
/// The backdrop that shows it draws this picture; every surface that stands on the shell frosts
/// the same picture, so a host of its own — the chrome drawing the overview — draws over exactly
/// what the backdrop under it shows, never a copy of its own.
///
/// The session holds one and hands it to both hosts; the picture, and the blur of it, are kept here
/// alone, so what one host draws and what the other draws over it never differ.
#[derive(Clone, Debug, Default)]
pub struct Source {
    state: Rc<RefCell<State>>,
}

#[derive(Debug, Default)]
struct State {
    picture: Option<ImageData>,
    /// The shell's gradient, from its first stop to its last, where it shows no picture.
    gradient: Option<(Color, Color)>,
}

impl Source {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take `picture` as the shell's wallpaper, or none for a shell without one.
    pub fn set(&self, picture: Option<ImageData>) {
        let mut state = self.state.borrow_mut();
        state.gradient = None;
        let known = state.picture.as_ref().map(|known| known.data.id());
        if known == picture.as_ref().map(|image| image.data.id()) {
            return;
        }
        state.picture = picture;
    }

    /// Take the shell's gradient, drawn corner to corner from `stops.0` to `stops.1`, as what it
    /// shows where it has no picture.
    pub fn set_gradient(&self, stops: (Color, Color)) {
        let mut state = self.state.borrow_mut();
        state.picture = None;
        state.gradient = Some(stops);
    }

    /// The picture the shell shows now, as the session holds it.
    #[must_use]
    pub fn picture(&self) -> Option<ImageData> {
        self.state.borrow().picture.clone()
    }

    /// The picture covering `rect`, blurred by `blur_px` logical pixels: what a surface resting on
    /// the shell shows of it, made once per picture, rectangle and blur ([`blurred_cover`] keeps
    /// it).
    #[must_use]
    pub fn cover(&self, rect: Rect, blur_px: f32) -> Option<(ImageData, Affine)> {
        let picture = self.state.borrow().picture.clone()?;
        blurred_cover(&picture, rect, blur_px)
    }
}

/// What the shell shows under `shape`, blurred: its picture covering the shape, its gradient
/// (already as smooth as its blur) across it, or `ground` where it shows neither or `blur` is off.
fn paint_shell(
    scene: &mut Scene,
    source: &Source,
    transform: Affine,
    shape: &impl Shape,
    blur: Option<f32>,
    ground: Color,
) {
    let Some(blur_px) = blur else {
        scene.fill(Fill::NonZero, transform, ground, None, shape);
        return;
    };
    let gradient = source.state.borrow().gradient;
    let (brush, brush_transform) =
        if let Some((image, placed)) = source.cover(shape.bounding_box(), blur_px) {
            let brush = Brush::Image(ImageBrush::new(image).with_extend(Extend::Pad));
            (brush, Some(transform.inverse() * placed))
        } else if let Some((from, to)) = gradient {
            let bounds = shape.bounding_box();
            let gradient = Gradient::new_linear(bounds.origin(), Point::new(bounds.x1, bounds.y1))
                .with_extend(Extend::Pad)
                .with_stops([from, to].as_slice());
            (Brush::Gradient(gradient), None)
        } else {
            scene.fill(Fill::NonZero, transform, ground, None, shape);
            return;
        };
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, shape);
    scene.fill(Fill::NonZero, transform, &brush, brush_transform, shape);
    scene.pop_layer();
}

/// Fill `shape` with the shell itself, blurred as a floating surface blurs it but under no tint: a
/// ground that stands for the shell rather than a surface over it. `ground` where the theme draws
/// floating surfaces opaque.
pub(crate) fn shell(
    scene: &mut Scene,
    source: &Source,
    transform: Affine,
    shape: &impl Shape,
    theme: &Theme,
    ground: Color,
) {
    let blur = super::blurs(Material::Floating, theme)
        .then_some(theme.tokens.colors.surfaces.material_blur_px);
    paint_shell(scene, source, transform, shape, blur, ground);
}

/// `image` covering `rect` as a shell paints a wallpaper (scaled to cover it, centred), blurred by
/// `blur_px` logical pixels: the blurred picture and the transform that places it over `rect`.
#[must_use]
pub fn blurred_cover(image: &ImageData, rect: Rect, blur_px: f32) -> Option<(ImageData, Affine)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || image.width == 0 || image.height == 0 {
        return None;
    }
    let blur = f64::from(blur_px.max(0.0));
    let texel = (blur / 3.0)
        .max(rect.width().max(rect.height()) / f64::from(MAX_EDGE))
        .max(1.0);
    let width = (rect.width() / texel).ceil().max(1.0) as u32;
    let height = (rect.height() / texel).ceil().max(1.0) as u32;
    let key = cover_key(image, rect, blur_px);
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
        if covers.len() >= 8 {
            covers.remove(0);
        }
        covers.push((key, blurred.clone()));
    });
    Some((blurred, placed))
}

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
