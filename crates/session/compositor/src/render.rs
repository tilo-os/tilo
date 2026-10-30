//! The scene of one output: what is drawn where, front first.
//!
//! Everything on screen is a buffer: client surfaces (their popups and
//! subsurfaces included) and the layers of hosted surfaces. The scene is
//! renderer-generic, so the same elements feed the CPU renderer of a headless
//! output and the GPU of a real one, and their damage tracking decides what
//! is redrawn.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size};

use crate::effects::WindowEffect;
use crate::state::State;

/// The colour behind every window: the desktop's background before a
/// wallpaper surface covers it.
pub const BACKGROUND: [f32; 4] = [0.102, 0.106, 0.149, 1.0];

smithay::backend::renderer::element::render_elements! {
    /// One drawable thing on an output.
    pub OutputRenderElement<R> where R: ImportAll + ImportMem;
    /// A client surface.
    Surface=WaylandSurfaceRenderElement<R>,
    /// A hosted surface's layer.
    Hosted=MemoryRenderBufferRenderElement<R>,
    /// The idle dim over everything, where no backlight dims.
    Dim=SolidColorRenderElement,
    /// A client surface under the magnifier.
    ZoomedSurface=RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    /// A hosted surface's layer under the magnifier.
    ZoomedHosted=RescaleRenderElement<MemoryRenderBufferRenderElement<R>>,
    /// A window drawn moved, scaled or faded, or pictured by a hosted
    /// surface.
    Transformed=crate::effects::TransformedElement<R>,
    /// A closed window's last frame, drawn moved, scaled or faded.
    Snapshot=crate::effects::SnapshotElement<R::TextureId>,
    /// A window's shadow and edge, behind it.
    Frame=crate::effects::FrameElement<R>,
}

/// `element` as the magnifier shows it: scaled by `zoom` about `centre`
/// (output physical pixels), which stays where it is on screen.
fn zoomed<R>(
    element: OutputRenderElement<R>,
    centre: Point<i32, Physical>,
    zoom: f64,
) -> OutputRenderElement<R>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    match element {
        OutputRenderElement::Surface(surface) => OutputRenderElement::ZoomedSurface(
            RescaleRenderElement::from_element(surface, centre, zoom),
        ),
        OutputRenderElement::Hosted(hosted) => OutputRenderElement::ZoomedHosted(
            RescaleRenderElement::from_element(hosted, centre, zoom),
        ),
        other => other,
    }
}

/// The idle dim's buffer of one output.
struct DimBuffer(std::cell::RefCell<SolidColorBuffer>);

/// The dim over all of `output`, when the scene is dimmed without a
/// backlight.
fn dim_element<R>(state: &State, output: &Output) -> Option<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let alpha = state.dim_alpha()?;
    let geometry = state.space.output_geometry(output)?;
    output
        .user_data()
        .insert_if_missing(|| DimBuffer(std::cell::RefCell::new(SolidColorBuffer::default())));
    let buffer = output.user_data().get::<DimBuffer>()?;
    let mut buffer = buffer.0.borrow_mut();
    buffer.update(geometry.size, Color32F::new(0.0, 0.0, 0.0, alpha));
    let scale = Scale::from(output.current_scale().fractional_scale());
    Some(OutputRenderElement::Dim(
        SolidColorRenderElement::from_buffer(
            &buffer,
            Point::<i32, Physical>::from((0, 0)),
            scale,
            1.0,
            Kind::Unspecified,
        ),
    ))
}

/// What lies below hosted surface `host`, front first: the hosts beneath
/// it and the windows, drawn at `scale` with `origin` (logical) at the
/// top left of the area `bounds` (physical), as its backdrop sees them —
/// the windows where their effects draw them, framed as they are drawn.
pub(crate) fn below_host<R>(
    renderer: &mut R,
    state: &State,
    host: usize,
    origin: Point<f64, Logical>,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let mut elements = Vec::new();
    if state.locked() {
        return elements;
    }
    for below in state.hosts.iter().take(host).rev() {
        for layer in below.layers().into_iter().rev() {
            let location =
                (Point::<i32, Logical>::from(layer.location).to_f64() - origin).to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location,
                layer.frame.buffer(),
                None,
                None,
                Some(Size::from(layer.size)),
                Kind::Unspecified,
            ) {
                Ok(element) => elements.push(OutputRenderElement::Hosted(element)),
                Err(error) => tracing::warn!(?error, "hosted layer: buffer not imported"),
            }
        }
    }
    elements.extend(window_elements(
        renderer,
        state,
        origin,
        scale,
        bounds,
        state.space.elements().rev().cloned().collect(),
    ));
    elements
}

/// The window layer, front first: `stack` (the mapped windows, bottom
/// first, reversed by the caller) and the windows that closed and are
/// still drawn where they stood, each drawn at `scale` with `origin` at
/// the top left of the area `bounds`.
fn window_elements<R>(
    renderer: &mut R,
    state: &State,
    origin: Point<f64, Logical>,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
    stack: Vec<Window>,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    // A window that closed keeps the place in the stack it had, so an
    // animation of it runs where it was drawn, not over the windows above.
    let len = stack.len();
    let mut closing: Vec<Vec<&crate::state::Closing>> = vec![Vec::new(); len + 1];
    for gone in &state.closing {
        if let Some(behind) = closing.get_mut(len.saturating_sub(gone.index).min(len)) {
            behind.push(gone);
        }
    }
    let mut elements = Vec::new();
    for (front, window) in stack.into_iter().enumerate() {
        let window = &window;
        for gone in closing.get(front).into_iter().flatten() {
            elements.extend(closing_elements(
                renderer, state, gone, origin, scale, bounds,
            ));
        }
        let Some(location) = state.space.element_location(window) else {
            continue;
        };
        let effect = state.effect_of(window);
        let framing = state.framing(window);
        // A window drawn as it stands keeps its buffer's own element, which
        // a display can scan out; anything else is drawn through its frame.
        if effect != WindowEffect::default() || !framing.frame.is_square() {
            elements.extend(crate::effects::with_effect(
                renderer,
                framing,
                crate::effects::Content::Window(window),
                location.to_f64() - origin,
                &effect,
                (bounds, scale),
            ));
            continue;
        }
        let at = location.to_f64() - window.geometry().loc.to_f64() - origin;
        elements.extend(window.render_elements::<OutputRenderElement<R>>(
            renderer,
            at.to_physical_precise_round(scale),
            Scale::from(scale),
            1.0,
        ));
        // What the window drew is kept while it stands, so its close has a
        // frame to animate even once its surface is gone.
        state.record_snapshot(renderer, window);
    }
    for gone in closing.get(len).into_iter().flatten() {
        elements.extend(closing_elements(
            renderer, state, gone, origin, scale, bounds,
        ));
    }
    elements
}

/// A window that closed, drawn from its last buffer with whatever effect
/// the desktop is animating it away with.
fn closing_elements<R>(
    renderer: &mut R,
    state: &State,
    closing: &crate::state::Closing,
    origin: Point<f64, Logical>,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let effect = state
        .effects
        .get(&closing.info.id)
        .copied()
        .unwrap_or_default();
    let framing = crate::effects::Framing {
        frame: closing.frame,
        images: &state.frame_images,
        snapshots: &state.snapshots,
        display: &state.display,
    };
    // The frame the window last drew, kept before its surface went: the
    // desktop animates that away.
    let Some(snapshot) = closing.snapshot.as_ref() else {
        return Vec::new();
    };
    crate::effects::with_effect(
        renderer,
        framing,
        crate::effects::Content::Snapshot(snapshot),
        closing.rect.loc.to_f64() - origin,
        &effect,
        (bounds, scale),
    )
}

/// The elements `output` shows, front first, in its physical pixels.
pub(crate) fn output_elements<R>(
    renderer: &mut R,
    state: &State,
    output: &Output,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let Some(geometry) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let dim = dim_element(state, output);
    let output_scale = output.current_scale().fractional_scale();
    let scale = Scale::from(output_scale);
    let bounds = Rectangle::from_size(geometry.size.to_physical_precise_round(output_scale));
    let window_of = |id: u64| {
        state.space.elements().find(|window| {
            crate::state::describe(&state.display, window).is_some_and(|info| info.id == id)
        })
    };
    let locked = state.locked();
    let mut elements: Vec<OutputRenderElement<R>> = dim.into_iter().collect();
    let hosted = |renderer: &mut R, elements: &mut Vec<OutputRenderElement<R>>, below: bool| {
        for host in state.hosts.iter().rev().filter(|host| {
            // While locked only the lock screen shows; until then never.
            host.lock_screen() == locked && host.below_windows() == below
        }) {
            for picture in host.window_pictures().iter().rev() {
                if let Some(window) = window_of(picture.window) {
                    elements.extend(crate::effects::pictured(
                        renderer,
                        state.framing(window),
                        window,
                        picture,
                        geometry.loc.to_f64(),
                        (bounds, output_scale),
                    ));
                }
            }
            for layer in host.layers().into_iter().rev() {
                let location: Point<i32, Physical> =
                    (Point::from(layer.location) - geometry.loc).to_physical_precise_round(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location.to_f64(),
                    layer.frame.buffer(),
                    None,
                    None,
                    Some(Size::from(layer.size)),
                    Kind::Unspecified,
                ) {
                    Ok(element) => elements.push(OutputRenderElement::Hosted(element)),
                    Err(error) => tracing::warn!(?error, "hosted layer: buffer not imported"),
                }
            }
        }
    };
    if locked {
        hosted(renderer, &mut elements, false);
        elements.extend(state.lock_elements(renderer, output));
        return elements;
    }
    elements.extend(state.drag_icon_elements(renderer, output));
    elements.extend(state.input_popup_elements(renderer, output));
    hosted(renderer, &mut elements, false);
    elements.extend(window_elements(
        renderer,
        state,
        geometry.loc.to_f64(),
        output_scale,
        bounds,
        state
            .space
            .elements_for_output(output)
            .rev()
            .cloned()
            .collect(),
    ));
    hosted(renderer, &mut elements, true);
    if state.zoom > 1.0 {
        // The magnifier scales the whole scene about the pointer, which
        // stays on what it points at.
        let centre: Point<i32, Physical> =
            (state.pointer - geometry.loc.to_f64()).to_physical_precise_round(scale);
        elements = elements
            .into_iter()
            .map(|element| zoomed(element, centre, state.zoom))
            .collect();
    }
    elements
}
