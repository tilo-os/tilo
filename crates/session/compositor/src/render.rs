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

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::output::Output;
use smithay::utils::{Physical, Point, Scale};

use crate::state::State;

smithay::backend::renderer::element::render_elements! {
    /// One drawable thing on an output.
    pub OutputRenderElement<R> where R: ImportAll + ImportMem;
    /// A client surface.
    Surface=WaylandSurfaceRenderElement<R>,
    /// A hosted surface's layer.
    Hosted=MemoryRenderBufferRenderElement<R>,
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
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mut elements = Vec::new();
    for host in state.hosts.iter().rev() {
        for layer in host.layers().into_iter().rev() {
            let location: Point<i32, Physical> =
                (Point::from(layer.location) - geometry.loc).to_physical_precise_round(scale);
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location.to_f64(),
                &layer.buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => elements.push(OutputRenderElement::Hosted(element)),
                Err(error) => tracing::warn!(?error, "hosted layer: buffer not imported"),
            }
        }
    }
    for window in state.space.elements_for_output(output).rev() {
        let Some(location) = state.space.element_location(window) else {
            continue;
        };
        let origin = location - window.geometry().loc - geometry.loc;
        elements.extend(window.render_elements::<OutputRenderElement<R>>(
            renderer,
            origin.to_physical_precise_round(scale),
            scale,
            1.0,
        ));
    }
    elements
}
