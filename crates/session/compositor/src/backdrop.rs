//! The blurred backdrop translucent chrome is drawn over.
//!
//! Floating chrome (quick settings, menus, the launcher, notifications) is
//! made of a material that shows a blur of what lies behind it. A hosted
//! app cannot see behind itself: the windows and the surfaces below are the
//! compositor's. So a hosted surface names the regions it draws the material
//! in ([`BackdropRequest`], from [`crate::SurfaceHost::backdrops`]) and the
//! compositor hands back, after each frame, a blurred image of what lies
//! below each ([`BackdropImage`], [`crate::SurfaceHost::set_backdrops`]),
//! drawn at the request's texel size, only when what lies below changed.
//! The app draws it in its material, under its tint, the next frame.
//!
//! The blur halves the image until its standard deviation is a few texels,
//! blurs it separably with a Gaussian kernel and scales it back: the
//! backdrop is a low-frequency signal, so this is indistinguishable from a
//! full-resolution blur at a fraction of the work.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "image arithmetic on texel indices and colour values of small backdrop images"
)]

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::{ExportMem, ImportAll, ImportMem, Offscreen, Renderer};
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

use crate::backend::BackendError;
use crate::render::{BACKGROUND, below_host};
use crate::state::State;

/// A region a hosted surface draws its material in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackdropRequest {
    /// The region, logical `(x, y, width, height)`.
    pub rect: (f64, f64, f64, f64),
    /// The size of the image it takes, in texels.
    pub texels: (u32, u32),
    /// The blur's standard deviation, in texels.
    pub blur: f64,
}

/// What lies below a [`BackdropRequest`]'s region, blurred: rows of
/// premultiplied R, G, B, A at its texel size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackdropImage {
    pub rgba: Vec<u8>,
    pub texels: (u32, u32),
}

/// The blur stops halving once its deviation is at most this many texels.
const MAX_SIGMA: f64 = 4.0;

/// Upper bound on halvings.
const MAX_HALVINGS: u32 = 6;

/// What one region's last image saw, to skip unchanged ones.
struct Watch {
    key: (usize, usize),
    request: BackdropRequest,
    tracker: OutputDamageTracker,
}

/// The regions served so far.
#[derive(Default)]
pub(crate) struct Backdrops {
    watches: Vec<Watch>,
}

/// Draw what lies below hosted surface `host`'s region `slot` and blur it;
/// `None` when nothing there changed since its last image.
pub(crate) fn draw<R, T>(
    renderer: &mut R,
    state: &mut State,
    (host, slot): (usize, usize),
    request: &BackdropRequest,
) -> Result<Option<BackdropImage>, BackendError>
where
    R: Renderer + ImportAll + ImportMem + Offscreen<T> + ExportMem,
    R::TextureId: Clone + Send + 'static,
{
    let (x, y, width, height) = request.rect;
    let (columns, rows) = request.texels;
    if width <= 0.0 || height <= 0.0 || columns == 0 || rows == 0 {
        return Ok(None);
    }
    let scale = f64::from(columns) / width;
    let size = Size::<i32, Physical>::from((
        i32::try_from(columns).unwrap_or(i32::MAX),
        i32::try_from(rows).unwrap_or(i32::MAX),
    ));
    let origin = Point::<f64, Logical>::from((x, y));
    let elements = below_host(
        renderer,
        state,
        host,
        origin,
        scale,
        Rectangle::from_size(size),
    );
    let watches = &mut state.backdrops.watches;
    let fresh = !watches
        .iter()
        .any(|watch| watch.key == (host, slot) && watch.request == *request);
    if fresh {
        watches.retain(|watch| watch.key != (host, slot));
        watches.push(Watch {
            key: (host, slot),
            request: *request,
            tracker: OutputDamageTracker::new(size, scale, Transform::Normal),
        });
    }
    let Some(watch) = watches.iter_mut().find(|watch| watch.key == (host, slot)) else {
        return Ok(None);
    };
    let changed = match watch.tracker.damage_output(1, &elements) {
        Ok((Some(damage), _)) => !damage.is_empty(),
        Ok((None, _)) => false,
        Err(_) => true,
    };
    if !fresh && !changed {
        return Ok(None);
    }
    let mut rgba = crate::capture::render_to_memory::<R, T, _>(
        renderer,
        size,
        scale,
        Transform::Normal,
        &elements,
        BACKGROUND,
    )
    .map_err(|error| BackendError(error.to_string()))?;
    blur(&mut rgba, (columns as usize, rows as usize), request.blur);
    Ok(Some(BackdropImage {
        rgba,
        texels: request.texels,
    }))
}

impl dyn crate::Backend {
    /// Hand every hosted surface that draws translucent material the
    /// backdrops that changed.
    pub(crate) fn serve_backdrops(&mut self, state: &mut State) {
        let requests: Vec<(usize, Vec<BackdropRequest>)> = state
            .hosts
            .iter()
            .enumerate()
            .map(|(index, host)| (index, host.backdrops()))
            .collect();
        let live: Vec<(usize, usize)> = requests
            .iter()
            .flat_map(|(host, list)| (0..list.len()).map(move |slot| (*host, slot)))
            .collect();
        state
            .backdrops
            .watches
            .retain(|watch| live.contains(&watch.key));
        for (host, list) in requests {
            if list.is_empty() {
                continue;
            }
            let images: Vec<Option<BackdropImage>> = list
                .iter()
                .enumerate()
                .map(
                    |(slot, request)| match self.backdrop(state, (host, slot), request) {
                        Ok(image) => image,
                        Err(error) => {
                            tracing::debug!(%error, "backdrop not drawn");
                            None
                        }
                    },
                )
                .collect();
            if images.iter().any(Option::is_some)
                && let Some(surface) = state.hosts.get_mut(host)
            {
                surface.set_backdrops(images);
            }
        }
    }
}

/// Blur premultiplied RGBA rows of `size` texels with a Gaussian of
/// standard deviation `sigma` texels, in place.
pub(crate) fn blur(rgba: &mut [u8], size: (usize, usize), sigma: f64) {
    let (width, height) = size;
    if sigma <= 0.0 || width == 0 || height == 0 || rgba.len() < width * height * 4 {
        return;
    }
    let mut halvings = 0;
    while sigma / f64::from(1_u32 << halvings) > MAX_SIGMA
        && halvings < MAX_HALVINGS
        && (width >> (halvings + 1)) > 0
        && (height >> (halvings + 1)) > 0
    {
        halvings += 1;
    }
    let factor = 1_usize << halvings;
    let small = (width.div_ceil(factor), height.div_ceil(factor));
    let mut image = downsample(rgba, size, factor, small);
    let kernel = kernel(sigma / factor as f64);
    convolve(&mut image, small, &kernel, true);
    convolve(&mut image, small, &kernel, false);
    upsample(&image, small, factor, rgba, size);
}

/// A normalized Gaussian kernel reaching three deviations each side.
fn kernel(sigma: f64) -> Vec<f32> {
    let reach = (sigma * 3.0).ceil().max(1.0) as usize;
    let weights: Vec<f64> = (0..=2 * reach)
        .map(|i| {
            let d = i as f64 - reach as f64;
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let total: f64 = weights.iter().sum();
    weights.iter().map(|w| (w / total) as f32).collect()
}

/// Averages of `factor` x `factor` blocks, as floats.
fn downsample(
    rgba: &[u8],
    (width, height): (usize, usize),
    factor: usize,
    (columns, rows): (usize, usize),
) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0_f32; 4]; columns * rows];
    for row in 0..rows {
        for column in 0..columns {
            let mut sum = [0.0_f32; 4];
            let mut count = 0.0_f32;
            for y in row * factor..((row + 1) * factor).min(height) {
                for x in column * factor..((column + 1) * factor).min(width) {
                    let at = (y * width + x) * 4;
                    if let Some(pixel) = rgba.get(at..at + 4) {
                        for (total, value) in sum.iter_mut().zip(pixel) {
                            *total += f32::from(*value);
                        }
                        count += 1.0;
                    }
                }
            }
            if let Some(slot) = out.get_mut(row * columns + column) {
                *slot = sum.map(|total| total / count.max(1.0));
            }
        }
    }
    out
}

/// One pass of a separable convolution, edges clamped.
fn convolve(image: &mut [[f32; 4]], (width, height): (usize, usize), kernel: &[f32], rows: bool) {
    let reach = kernel.len() / 2;
    let (lines, length) = if rows {
        (height, width)
    } else {
        (width, height)
    };
    let index = |line: usize, along: usize| {
        if rows {
            line * width + along
        } else {
            along * width + line
        }
    };
    let mut scratch = vec![[0.0_f32; 4]; length];
    for line in 0..lines {
        for (along, out) in scratch.iter_mut().enumerate() {
            let mut sum = [0.0_f32; 4];
            for (tap, weight) in kernel.iter().enumerate() {
                let source = (along + tap).saturating_sub(reach).min(length - 1);
                if let Some(pixel) = image.get(index(line, source)) {
                    for (total, value) in sum.iter_mut().zip(pixel) {
                        *total += value * weight;
                    }
                }
            }
            *out = sum;
        }
        for (along, value) in scratch.iter().enumerate() {
            if let Some(slot) = image.get_mut(index(line, along)) {
                *slot = *value;
            }
        }
    }
}

/// Scale `image` back up by `factor` (bilinear) into `rgba`.
fn upsample(
    image: &[[f32; 4]],
    (columns, rows): (usize, usize),
    factor: usize,
    rgba: &mut [u8],
    (width, height): (usize, usize),
) {
    let sample = |x: usize, y: usize| image.get(y.min(rows - 1) * columns + x.min(columns - 1));
    for y in 0..height {
        let fy = ((y as f64 + 0.5) / factor as f64 - 0.5).max(0.0);
        let (y0, ty) = (fy.floor() as usize, (fy - fy.floor()) as f32);
        for x in 0..width {
            let fx = ((x as f64 + 0.5) / factor as f64 - 0.5).max(0.0);
            let (x0, tx) = (fx.floor() as usize, (fx - fx.floor()) as f32);
            let (Some(a), Some(b), Some(c), Some(d)) = (
                sample(x0, y0),
                sample(x0 + 1, y0),
                sample(x0, y0 + 1),
                sample(x0 + 1, y0 + 1),
            ) else {
                continue;
            };
            let at = (y * width + x) * 4;
            if let Some(pixel) = rgba.get_mut(at..at + 4) {
                let corners = a.iter().zip(b).zip(c.iter().zip(d));
                for (out, ((a, b), (c, d))) in pixel.iter_mut().zip(corners) {
                    let top = a + (b - a) * tx;
                    let bottom = c + (d - c) * tx;
                    *out = (top + (bottom - top) * ty).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_image_stays_flat_and_an_edge_spreads() {
        let (width, height) = (64, 16);
        let mut flat = [10_u8, 20, 30, 255].repeat(width * height);
        let before = flat.clone();
        blur(&mut flat, (width, height), 12.0);
        assert!(
            flat.iter().zip(&before).all(|(a, b)| a.abs_diff(*b) <= 1),
            "a blur of one colour is that colour"
        );

        // Left half black, right half white.
        let mut edge: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                let v = if i % width < width / 2 { 0 } else { 255 };
                [v, v, v, 255]
            })
            .collect();
        blur(&mut edge, (width, height), 6.0);
        let at = |x: usize| edge[(8 * width + x) * 4];
        assert!(at(0) < 10 && at(63) > 245, "far from the edge it holds");
        assert!(at(30) > 40 && at(33) < 215, "near it, it spreads");
        assert!(at(31) <= at(32), "and still rises left to right");
    }

    #[test]
    fn the_kernel_sums_to_one() {
        let kernel = kernel(2.5);
        assert_eq!(kernel.len(), 2 * 8 + 1);
        assert!((kernel.iter().sum::<f32>() - 1.0).abs() < 1e-5);
    }
}
