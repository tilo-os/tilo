//! Windows drawn other than where they stand: animated, faded, pictured,
//! and framed the way the desktop draws windows.
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
//!
//! Every window is also framed ([`crate::WindowFrame`]): its corners are
//! cut to the desktop's rounding, and the shadow and hairline that lift it
//! off the ground are drawn behind it. Both follow the effect, so a window
//! that is moved, scaled or faded keeps its corners and its shadow with it,
//! and a window pictured small is framed small.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use smithay::backend::renderer::element::AsRenderElements as _;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::Window;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size};

use crate::render::OutputRenderElement;
use crate::window_frame::{self, FrameImages, WindowFrame};

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

/// How the desktop frames windows, and the shadows and edges drawn so far.
#[derive(Clone, Copy)]
pub(crate) struct Framing<'a> {
    /// What every window of this drawing gets.
    pub(crate) frame: WindowFrame,
    /// The frames drawn so far, by window size and output scale.
    pub(crate) images: &'a FrameImages,
}

/// One drawn piece of a window moved, scaled and clipped, its corners cut
/// to the desktop's rounding.
pub type TransformedElement<R> =
    RoundedElement<RescaleRenderElement<WaylandSurfaceRenderElement<R>>>;

/// A window's shadow and edge, drawn where the window is drawn.
pub type FrameElement<R> =
    CropRenderElement<RescaleRenderElement<MemoryRenderBufferRenderElement<R>>>;

/// An element with its corners cut to a rounded rect.
///
/// The cut is made where the element is drawn ([`RenderElement::draw`]), by
/// drawing its rows: the corner strips in as many rows as the arc is deep
/// and the rest once. Every renderer draws a texture into a rows worth of a
/// destination the same way, so this needs nothing of the GPU and nothing
/// of the CPU renderer's shape handling.
#[derive(Debug)]
pub struct RoundedElement<E> {
    element: E,
    /// The corner radius, physical pixels.
    radius: f64,
    /// What it may cover, physical.
    clip: Rectangle<i32, Physical>,
}

impl<E> RoundedElement<E> {
    /// `element`, drawn with corners rounded by `radius` (physical pixels),
    /// covering no more than `clip`.
    pub fn new(element: E, radius: f64, clip: Rectangle<i32, Physical>) -> Self {
        Self {
            element,
            radius,
            clip,
        }
    }

    /// The element inside.
    #[must_use]
    pub fn inner(&self) -> &E {
        &self.element
    }
}

impl<E: Element> Element for RoundedElement<E> {
    fn id(&self) -> &Id {
        self.element.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.element.current_commit()
    }

    fn transform(&self) -> smithay::utils::Transform {
        self.element.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.element.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element
            .geometry(scale)
            .intersection(self.clip)
            .unwrap_or_default()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.element.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // The corners let what is behind them show.
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.element.alpha()
    }

    fn kind(&self) -> Kind {
        self.element.kind()
    }
}

impl<R, E> RenderElement<R> for RoundedElement<E>
where
    R: Renderer,
    E: RenderElement<R>,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let bands = window_frame::bands(dst, self.radius);
        if bands.len() <= 1 {
            return self.element.draw(frame, src, dst, damage, opaque_regions);
        }
        // `damage` and `opaque_regions` are relative to `dst`; each row is
        // handed its own share of them.
        let within = |rect: Rectangle<i32, Physical>, row: Rectangle<i32, Physical>| {
            let absolute = Rectangle::new(rect.loc + dst.loc, rect.size);
            absolute
                .intersection(row)
                .map(|part| Rectangle::new(part.loc - row.loc, part.size))
        };
        for row in bands {
            let damage: Vec<Rectangle<i32, Physical>> = damage
                .iter()
                .copied()
                .filter_map(|rect| within(rect, row))
                .collect();
            if damage.is_empty() {
                continue;
            }
            let opaque: Vec<Rectangle<i32, Physical>> = opaque_regions
                .iter()
                .copied()
                .filter_map(|rect| within(rect, row))
                .collect();
            // The row's share of the source, so the buffer is drawn as one.
            let across = |length: i32, whole: i32, of: f64| {
                if whole > 0 {
                    of * f64::from(length) / f64::from(whole)
                } else {
                    0.0
                }
            };
            let left = across(row.loc.x - dst.loc.x, dst.size.w, src.size.w);
            let top = across(row.loc.y - dst.loc.y, dst.size.h, src.size.h);
            let width = across(row.size.w, dst.size.w, src.size.w);
            let height = across(row.size.h, dst.size.h, src.size.h);
            let row_src = Rectangle::<f64, Buffer>::new(
                src.loc + Point::from((left, top)),
                Size::from((width, height)),
            );
            self.element.draw(frame, row_src, row, &damage, &opaque)?;
        }
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// How a window's content is placed on an output.
struct Placement {
    /// Its geometry's top left, output-local logical.
    at: Point<f64, Logical>,
    /// What it is scaled about, physical.
    pivot: Point<i32, Physical>,
    /// By how much, about `pivot`.
    scale: f64,
    /// Its opacity, 0 to 1.
    opacity: f32,
    /// What it may cover, physical.
    clip: Rectangle<i32, Physical>,
    /// Its corner radius before the effect, logical.
    radius: f64,
}

/// `window`'s content and frame placed as `placement` says on an output of
/// `output_scale`, front first.
fn placed<R>(
    renderer: &mut R,
    framing: Framing<'_>,
    window: &Window,
    output_scale: f64,
    placement: &Placement,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let geometry = window.geometry();
    let origin = (placement.at - geometry.loc.to_f64()).to_physical_precise_round(output_scale);
    let unscaled = Rectangle::<i32, Physical>::new(
        placement.at.to_physical_precise_round(output_scale),
        geometry.size.to_physical_precise_round(output_scale),
    );
    let radius = placement.radius * placement.scale * output_scale;
    let mut elements: Vec<OutputRenderElement<R>> = window
        .render_elements::<WaylandSurfaceRenderElement<R>>(
            renderer,
            origin,
            Scale::from(output_scale),
            placement.opacity,
        )
        .into_iter()
        .map(|element| {
            let scaled =
                RescaleRenderElement::from_element(element, placement.pivot, placement.scale);
            OutputRenderElement::Transformed(RoundedElement::new(scaled, radius, placement.clip))
        })
        .collect();
    // The shadow and the edge sit under the window, so they come after it.
    let unscaled_size = geometry.size.to_f64();
    let image = framing.images.image(
        &framing.frame,
        (unscaled_size.w, unscaled_size.h),
        output_scale,
    );
    if let Some(image) = image {
        let at = unscaled.loc.to_f64() + Point::<f64, Physical>::from(image.origin);
        let logical = Size::<i32, Logical>::from((
            window_frame::pixels(image.size.0 / output_scale),
            window_frame::pixels(image.size.1 / output_scale),
        ));
        let element = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            at,
            &image.buffer,
            Some(placement.opacity),
            None,
            Some(logical),
            Kind::Unspecified,
        );
        match element {
            Ok(element) => {
                let scaled =
                    RescaleRenderElement::from_element(element, placement.pivot, placement.scale);
                if let Some(element) =
                    CropRenderElement::from_element(scaled, output_scale, placement.clip)
                {
                    elements.push(OutputRenderElement::Frame(element));
                }
            }
            Err(error) => tracing::warn!(?error, "window frame: buffer not imported"),
        }
    }
    elements
}

/// `window`, standing at `location` (its geometry's top left, output-local
/// logical) drawn with `effect` on an output of `output` (physical size,
/// scale).
pub(crate) fn with_effect<R>(
    renderer: &mut R,
    framing: Framing<'_>,
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
    placed(
        renderer,
        framing,
        window,
        output_scale,
        &Placement {
            at,
            pivot: centre.to_physical_precise_round(output_scale),
            scale: effect.scale,
            opacity: effect.opacity.min(1.0),
            clip: bounds,
            radius: framing.frame.radius,
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
    framing: Framing<'_>,
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
    // scaled about that corner. The shadow is scaled with it, so a
    // thumbnail carries the window's own lift.
    placed(
        renderer,
        framing,
        window,
        output_scale,
        &Placement {
            at,
            pivot: at.to_physical_precise_round(output_scale),
            scale,
            opacity: picture.opacity.min(1.0),
            clip,
            radius: framing.frame.radius,
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
