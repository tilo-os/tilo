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
use smithay::output::Output;
use smithay::utils::{Physical, Point, Rectangle, Scale, Size};

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
    if state.locked() {
        return dim
            .into_iter()
            .chain(state.lock_elements(renderer, output))
            .collect();
    }
    let output_scale = output.current_scale().fractional_scale();
    let scale = Scale::from(output_scale);
    let bounds = Rectangle::from_size(geometry.size.to_physical_precise_round(output_scale));
    let window_of = |id: u64| {
        state
            .space
            .elements()
            .find(|window| crate::state::describe(window).is_some_and(|info| info.id == id))
    };
    let mut elements: Vec<OutputRenderElement<R>> = dim.into_iter().collect();
    elements.extend(state.drag_icon_elements(renderer, output));
    elements.extend(state.input_popup_elements(renderer, output));
    let hosted = |renderer: &mut R, elements: &mut Vec<OutputRenderElement<R>>, below: bool| {
        for host in state
            .hosts
            .iter()
            .rev()
            .filter(|host| host.below_windows() == below)
        {
            for picture in host.window_pictures().iter().rev() {
                if let Some(window) = window_of(picture.window) {
                    elements.extend(crate::effects::pictured(
                        renderer,
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
                    &layer.buffer,
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
    hosted(renderer, &mut elements, false);
    for window in state.space.elements_for_output(output).rev() {
        let Some(location) = state.space.element_location(window) else {
            continue;
        };
        let effect = crate::state::describe(window).and_then(|info| state.effects.get(&info.id));
        if let Some(effect) = effect {
            let Some(standing) = state.space.element_geometry(window) else {
                continue;
            };
            elements.extend(crate::effects::with_effect(
                renderer,
                window,
                (standing.loc - geometry.loc).to_f64(),
                effect,
                (bounds, output_scale),
            ));
            continue;
        }
        let origin = location - window.geometry().loc - geometry.loc;
        elements.extend(window.render_elements::<OutputRenderElement<R>>(
            renderer,
            origin.to_physical_precise_round(scale),
            scale,
            1.0,
        ));
    }
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
