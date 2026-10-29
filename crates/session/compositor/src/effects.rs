//! Windows drawn other than where they stand: animated, faded, pictured.
//!
//! The desktop animates foreign windows (opening, closing, moving between
//! strips and workspaces, coming into the overview) by telling the
//! compositor each frame how to draw each one ([`WindowEffect`]: an
//! offset, a scale about the window's centre and an opacity); the curves
//! and clocks are the toolkit's, in the session, not the compositor's.
//! A hosted surface that shows other clients' windows (the overview's
//! thumbnails, a switcher card) names them and the rects they rest at
//! ([`WindowPicture`], from [`crate::SurfaceHost::window_pictures`]); the
//! compositor draws each window's live buffer there, scaled to fit, in
//! front of that surface's layers. Neither changes where input lands: a
//! window takes the pointer where it stands.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use smithay::backend::renderer::element::AsRenderElements as _;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::Window;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};

use crate::render::OutputRenderElement;

/// How one window is drawn this frame instead of as it stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowEffect {
    /// Moved by this much, logical pixels.
    pub offset: (f64, f64),
    /// Scaled by this much about the window's centre (1: as it is).
    pub scale: f64,
    /// Its opacity, 0 (invisible) to 1.
    pub opacity: f32,
}

impl Default for WindowEffect {
    fn default() -> Self {
        Self {
            offset: (0.0, 0.0),
            scale: 1.0,
            opacity: 1.0,
        }
    }
}

/// A window a hosted surface pictures: its live buffer, scaled to fit
/// `rect` (centred, its aspect kept), clipped to `clip`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowPicture {
    /// The window, by [`crate::WindowInfo::id`].
    pub window: u64,
    /// Where the picture rests, logical `(x, y, width, height)`.
    pub rect: (f64, f64, f64, f64),
    /// Its opacity, 0 to 1.
    pub opacity: f32,
    /// What it may cover, logical `(x, y, width, height)`; all of the
    /// output when `None`.
    pub clip: Option<(f64, f64, f64, f64)>,
}

/// One drawn piece of a window moved, scaled and clipped.
pub type TransformedElement<R> =
    CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<R>>>;

/// How a window's elements are placed (all output-local).
struct Placement {
    /// Its geometry's top left, logical.
    at: Point<f64, Logical>,
    /// What it is scaled about, physical.
    pivot: Point<i32, Physical>,
    scale: f64,
    opacity: f32,
    /// What it may cover, physical.
    clip: Rectangle<i32, Physical>,
}

/// `window`'s elements placed as `placement` says on an output of
/// `output_scale`.
fn transformed<R>(
    renderer: &mut R,
    window: &Window,
    output_scale: f64,
    placement: &Placement,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let origin =
        (placement.at - window.geometry().loc.to_f64()).to_physical_precise_round(output_scale);
    window
        .render_elements::<WaylandSurfaceRenderElement<R>>(
            renderer,
            origin,
            Scale::from(output_scale),
            placement.opacity,
        )
        .into_iter()
        .filter_map(|element| {
            let scaled =
                RescaleRenderElement::from_element(element, placement.pivot, placement.scale);
            CropRenderElement::from_element(scaled, output_scale, placement.clip)
        })
        .map(OutputRenderElement::Transformed)
        .collect()
}

/// `window`, standing at `location` (its geometry's top left, output-local
/// logical) and `size`, drawn with `effect` on an output of `output`
/// (physical size, scale).
pub(crate) fn with_effect<R>(
    renderer: &mut R,
    window: &Window,
    location: Point<f64, Logical>,
    effect: &WindowEffect,
    output: (Rectangle<i32, Physical>, f64),
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let (bounds, output_scale) = output;
    if effect.opacity <= 0.0 || effect.scale <= 0.0 {
        return Vec::new();
    }
    let size = window.geometry().size.to_f64();
    let at = location + Point::from(effect.offset);
    let centre = at + Point::from((size.w / 2.0, size.h / 2.0));
    transformed(
        renderer,
        window,
        output_scale,
        &Placement {
            at,
            pivot: centre.to_physical_precise_round(output_scale),
            scale: effect.scale,
            opacity: effect.opacity.min(1.0),
            clip: bounds,
        },
    )
}

/// The scale and top left (logical) that fit a window of `size` into
/// `rect`, centred, its aspect kept; `None` for an empty window or rect.
pub(crate) fn fit(
    size: (f64, f64),
    rect: (f64, f64, f64, f64),
) -> Option<(f64, Point<f64, Logical>)> {
    let (x, y, width, height) = rect;
    if size.0 <= 0.0 || size.1 <= 0.0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = (width / size.0).min(height / size.1);
    let left = x + (width - size.0 * scale) / 2.0;
    let top = y + (height - size.1 * scale) / 2.0;
    Some((scale, Point::from((left, top))))
}

/// `window` pictured as `picture` says, on an output whose top left in the
/// logical layout is `output_origin`.
pub(crate) fn pictured<R>(
    renderer: &mut R,
    window: &Window,
    picture: &WindowPicture,
    output_origin: Point<f64, Logical>,
    output: (Rectangle<i32, Physical>, f64),
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let (bounds, output_scale) = output;
    let size = window.geometry().size.to_f64();
    let Some((scale, top_left)) = fit((size.w, size.h), picture.rect) else {
        return Vec::new();
    };
    if picture.opacity <= 0.0 {
        return Vec::new();
    }
    let at = top_left - output_origin;
    let clip = picture.clip.map_or(Some(bounds), |(x, y, width, height)| {
        let logical = Rectangle::<f64, Logical>::new(
            Point::from((x, y)) - output_origin,
            (width, height).into(),
        );
        logical
            .to_physical(output_scale)
            .to_i32_round()
            .intersection(bounds)
    });
    let Some(clip) = clip else {
        return Vec::new();
    };
    // Drawn at its own size with its top left on the picture's, then
    // scaled about that corner.
    transformed(
        renderer,
        window,
        output_scale,
        &Placement {
            at,
            pivot: at.to_physical_precise_round(output_scale),
            scale,
            opacity: picture.opacity.min(1.0),
            clip,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_fits_its_picture_centred_with_its_aspect_kept() {
        let (scale, at) = fit((400.0, 200.0), (10.0, 20.0, 100.0, 100.0)).unwrap();
        assert!((scale - 0.25).abs() < 1e-9);
        assert_eq!((at.x, at.y), (10.0, 45.0));
        let (scale, at) = fit((100.0, 400.0), (0.0, 0.0, 100.0, 100.0)).unwrap();
        assert!((scale - 0.25).abs() < 1e-9);
        assert_eq!((at.x, at.y), (37.5, 0.0));
        assert!(fit((0.0, 10.0), (0.0, 0.0, 10.0, 10.0)).is_none());
        assert!(fit((10.0, 10.0), (0.0, 0.0, 0.0, 10.0)).is_none());
    }
}
