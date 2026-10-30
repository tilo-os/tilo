//! `zwlr_layer_shell_v1`: surfaces an output places itself, on a layer of
//! its own instead of by the window policy.
//!
//! The session's own surfaces are ordinary toolkit apps and ordinary
//! Wayland clients: the desk below the windows, the bar and its panels
//! above them. Each asks for a layer and an edge of an output to hang from;
//! the compositor places it there, draws it with the other clients' buffers
//! and sends it the input that lands on it. Only clients the session
//! connected as trusted may ask: everyone else is not offered the global,
//! as with the session lock.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "layer geometry: logical coordinates of an output and the surfaces on it, far from i32 limits"
)]

use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::{Kind, RenderElementStates};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::WindowSurfaceType;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind as FeedbackKind;
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree,
    surface_presentation_feedback_flags_from_states, take_presentation_feedback_surface_tree,
    under_from_surface_tree,
};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Scale, Size};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::wayland::compositor::{get_parent, with_states};
use smithay::wayland::shell::wlr_layer::{
    Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceCachedState,
    LayerSurfaceConfigure, WlrLayerShellHandler, WlrLayerShellState,
};

use crate::render::OutputRenderElement;
use crate::state::{ClientState, State};

/// The layer surfaces, in the order their clients made them: of two on one
/// layer, the later one is above.
pub(crate) struct Layers {
    shell: WlrLayerShellState,
    surfaces: Vec<Placed>,
}

/// One layer surface and where it is on its output.
struct Placed {
    surface: LayerSurface,
    /// The output it asked for, until an output exists to put it on.
    output: Option<Output>,
    layer: Layer,
    /// Its rectangle in the logical layout, from its anchors, margins and
    /// size on the output it is on; empty before it has an output.
    rect: Rectangle<i32, Logical>,
}

impl Placed {
    /// Whether the client drew it: a layer surface shows nothing before its
    /// first buffer.
    fn shown(&self) -> bool {
        with_renderer_surface_state(self.surface.wl_surface(), |state| state.buffer().is_some())
            .unwrap_or(false)
    }

    /// What its client asked of it: its anchors, margins and size.
    fn request(&self) -> LayerSurfaceCachedState {
        with_states(self.surface.wl_surface(), |states| {
            *states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
        })
    }
}

impl Layers {
    /// The global, offered to trusted clients only.
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        let shell = WlrLayerShellState::new_with_filter::<State, _>(display, |client: &Client| {
            client
                .get_data::<ClientState>()
                .is_some_and(ClientState::trusted)
        });
        Self {
            shell,
            surfaces: Vec::new(),
        }
    }

    /// The index of the layer surface whose root surface `surface` is.
    fn index_of(&self, surface: &WlSurface) -> Option<usize> {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        self.surfaces
            .iter()
            .position(|placed| placed.surface.wl_surface() == &root)
    }

    /// Forget the layer surfaces whose clients are gone.
    pub(crate) fn refresh(&mut self) {
        self.surfaces.retain(|placed| placed.surface.alive());
    }

    /// Whether `surface` is (part of) a layer surface's tree.
    pub(crate) fn holds(&self, surface: &WlSurface) -> bool {
        self.index_of(surface).is_some()
    }

    /// The layer surfaces on `output` that `want` takes, bottom first.
    fn on<'a>(
        &'a self,
        output: &'a Output,
        want: impl Fn(Layer) -> bool + 'a,
    ) -> impl DoubleEndedIterator<Item = &'a Placed> + 'a {
        self.surfaces
            .iter()
            .filter(move |placed| placed.output.as_ref() == Some(output))
            .filter(move |placed| want(placed.layer))
    }

    /// The layer surfaces under `point` (logical) on `output`, top first:
    /// the ones above the windows when `above`, else the ones below them.
    fn under(
        &self,
        output: &Output,
        point: Point<f64, Logical>,
        above: bool,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let want = |layer: Layer| {
            matches!(
                (above, layer),
                (true, Layer::Overlay | Layer::Top) | (false, Layer::Background | Layer::Bottom)
            )
        };
        self.on(output, want)
            .rev()
            .filter(|placed| placed.shown())
            .find_map(|placed| {
                let rect = placed.rect;
                if !rect.to_f64().contains(point) {
                    return None;
                }
                let local = point - rect.loc.to_f64();
                let (surface, offset) = under_from_surface_tree(
                    placed.surface.wl_surface(),
                    local,
                    rect.loc,
                    WindowSurfaceType::ALL,
                )?;
                Some((surface, offset.to_f64()))
            })
    }
}

impl State {
    /// A layer surface was made: put it on the output it asked for (or the
    /// first one there is) and tell its client the size it gets.
    pub(crate) fn add_layer_surface(
        &mut self,
        surface: LayerSurface,
        output: Option<&WlOutput>,
        layer: Layer,
    ) {
        let output = output.and_then(Output::from_resource);
        self.layers.surfaces.push(Placed {
            surface,
            output,
            layer,
            rect: Rectangle::default(),
        });
        let index = self.layers.surfaces.len().saturating_sub(1);
        self.place_layer(index, true);
        self.mark_redraw_all();
    }

    /// Put the layer surface at `index` on its output and, with `send`, give
    /// its client the size it gets.
    fn place_layer(&mut self, index: usize, send: bool) {
        let Some(placed) = self.layers.surfaces.get(index) else {
            return;
        };
        // A surface that named no output goes on the first one there is.
        let output = placed
            .output
            .clone()
            .or_else(|| self.space.outputs().next().cloned());
        let geometry = output
            .as_ref()
            .and_then(|output| self.space.output_geometry(output));
        let Some(placed) = self.layers.surfaces.get_mut(index) else {
            return;
        };
        placed.output = output;
        let Some(geometry) = geometry else {
            return;
        };
        let rect = placement(&placed.request(), geometry);
        placed.rect = rect;
        if !send {
            return;
        }
        placed.surface.with_pending_state(|state| {
            state.size = Some(rect.size);
        });
        if placed.surface.has_pending_changes() {
            placed.surface.send_configure();
        }
    }

    /// A layer surface's client committed on it: it may show now, at the
    /// size it was given.
    pub(crate) fn layer_committed(&mut self, surface: &WlSurface) {
        let Some(index) = self.layers.index_of(surface) else {
            return;
        };
        self.place_layer(index, true);
        self.layer_keyboard_focus(index);
        self.mark_redraw_all();
    }

    /// Place every layer surface again: an output came, went or changed.
    pub(crate) fn place_layers(&mut self) {
        for index in 0..self.layers.surfaces.len() {
            self.place_layer(index, true);
        }
    }

    /// The layer surface `surface` acked a configure: it may show now, and
    /// a surface that asks for the keyboard takes it.
    pub(crate) fn layer_configured(
        &mut self,
        surface: &WlSurface,
        _configure: &LayerSurfaceConfigure,
    ) {
        let Some(index) = self.layers.index_of(surface) else {
            return;
        };
        self.place_layer(index, true);
        self.layer_keyboard_focus(index);
        self.mark_redraw_all();
    }

    /// A layer surface went away: it takes no more input.
    pub(crate) fn remove_layer_surface(&mut self, surface: &WlSurface) {
        let Some(index) = self.layers.index_of(surface) else {
            return;
        };
        let root = self
            .layers
            .surfaces
            .get(index)
            .map(|placed| placed.surface.wl_surface().clone());
        self.layers.surfaces.remove(index);
        if let Some(root) = root
            && self
                .seat
                .get_keyboard()
                .is_some_and(|keyboard| keyboard.current_focus().as_ref() == Some(&root))
            && let Some(keyboard) = self.seat.get_keyboard()
        {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
        self.mark_redraw_all();
    }

    /// Give the keyboard to the layer surface at `index` when it asks for it
    /// exclusively, as a lock screen or a search field does.
    fn layer_keyboard_focus(&mut self, index: usize) {
        let Some(placed) = self.layers.surfaces.get(index) else {
            return;
        };
        if placed.request().keyboard_interactivity != KeyboardInteractivity::Exclusive {
            return;
        }
        if !matches!(placed.layer, Layer::Top | Layer::Overlay) || !placed.shown() || self.locked()
        {
            return;
        }
        let focus = placed.surface.wl_surface().clone();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(focus), SERIAL_COUNTER.next_serial());
        }
    }

    /// A pointer press landed at `point`: a layer surface there that takes
    /// the keyboard while pressed gets it.
    pub(crate) fn layer_focus_on_press(&mut self, point: Point<f64, Logical>) {
        if self.locked() {
            return;
        }
        let Some(output) = self.output_under(point) else {
            return;
        };
        let Some((surface, _)) = self.layers.under(&output, point, true) else {
            return;
        };
        let Some(index) = self.layers.index_of(&surface) else {
            return;
        };
        let Some(placed) = self.layers.surfaces.get(index) else {
            return;
        };
        if placed.request().keyboard_interactivity != KeyboardInteractivity::OnDemand {
            return;
        }
        let focus = placed.surface.wl_surface().clone();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(focus), SERIAL_COUNTER.next_serial());
        }
    }

    /// The output whose rectangle holds `point`.
    fn output_under(&self, point: Point<f64, Logical>) -> Option<Output> {
        self.space
            .outputs()
            .find(|output| {
                self.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.to_f64().contains(point))
            })
            .cloned()
    }

    /// The layer surface under `point` and where its surface starts, for the
    /// pointer: the ones above the windows, or the ones below them.
    pub(crate) fn layer_surface_under(
        &self,
        point: Point<f64, Logical>,
        above: bool,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.locked() {
            return None;
        }
        let output = self.output_under(point)?;
        self.layers.under(&output, point, above)
    }

    /// The elements the layer surfaces above or below the windows show on
    /// `output`: drawn with the other buffers of the scene, at the scale the
    /// output shows them.
    pub(crate) fn layer_elements<R>(
        &self,
        renderer: &mut R,
        output: &Output,
        above: bool,
        geometry: Rectangle<i32, Logical>,
        scale: f64,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        if self.locked() {
            return Vec::new();
        }
        let want = |layer: Layer| {
            matches!(
                (above, layer),
                (true, Layer::Overlay | Layer::Top) | (false, Layer::Background | Layer::Bottom)
            )
        };
        let mut elements = Vec::new();
        for placed in self.layers.on(output, want).filter(|placed| placed.shown()) {
            let location = (placed.rect.loc - geometry.loc).to_f64();
            elements.extend(render_elements_from_surface_tree::<R, _>(
                renderer,
                placed.surface.wl_surface(),
                location.to_physical_precise_round(scale),
                Scale::from(scale),
                1.0,
                Kind::Unspecified,
            ));
        }
        elements
    }

    /// Send the layer surfaces on `output` their frame callbacks and take
    /// their presentation feedback with the other buffers of the frame.
    pub(crate) fn layer_frame_drawn(
        &self,
        output: &Output,
        time: std::time::Duration,
        feedback: &mut OutputPresentationFeedback,
        states: Option<&RenderElementStates>,
    ) {
        for placed in self
            .layers
            .surfaces
            .iter()
            .filter(|placed| placed.output.as_ref() == Some(output) && placed.shown())
        {
            let surface = placed.surface.wl_surface();
            send_frames_surface_tree(
                surface,
                output,
                time,
                Some(std::time::Duration::ZERO),
                |_, _| Some(output.clone()),
            );
            take_presentation_feedback_surface_tree(
                surface,
                feedback,
                |_, _| Some(output.clone()),
                |surface, _| {
                    states.map_or_else(FeedbackKind::empty, |states| {
                        surface_presentation_feedback_flags_from_states(surface, states)
                    })
                },
            );
        }
    }

    /// What the layer surfaces on `output` keep for themselves at its
    /// edges: the areas a desktop should keep windows out of, from the
    /// exclusive zones they asked for.
    pub(crate) fn layer_reserved(&self, output: &Output) -> Vec<Rectangle<i32, Logical>> {
        let Some(geometry) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        self.layers
            .on(output, |_| true)
            .filter(|placed| placed.shown())
            .filter_map(|placed| {
                let request = placed.request();
                let ExclusiveZone::Exclusive(zone) = request.exclusive_zone else {
                    return None;
                };
                let zone = i32::try_from(zone).unwrap_or(i32::MAX);
                let anchor = request.anchor;
                // Only a zone along one edge keeps an area: a bar along the
                // top, a rail along a side.
                let top = anchor.contains(Anchor::TOP) && !anchor.contains(Anchor::BOTTOM);
                let bottom = anchor.contains(Anchor::BOTTOM) && !anchor.contains(Anchor::TOP);
                let left = anchor.contains(Anchor::LEFT) && !anchor.contains(Anchor::RIGHT);
                let right = anchor.contains(Anchor::RIGHT) && !anchor.contains(Anchor::LEFT);
                let along_top = zone.saturating_add(request.margin.top);
                let along_bottom = zone.saturating_add(request.margin.bottom);
                let along_left = zone.saturating_add(request.margin.left);
                let along_right = zone.saturating_add(request.margin.right);
                Some(if top {
                    Rectangle::new(geometry.loc, Size::from((geometry.size.w, along_top)))
                } else if bottom {
                    Rectangle::new(
                        Point::from((
                            geometry.loc.x,
                            geometry.loc.y + geometry.size.h - along_bottom,
                        )),
                        Size::from((geometry.size.w, along_bottom)),
                    )
                } else if left {
                    Rectangle::new(geometry.loc, Size::from((along_left, geometry.size.h)))
                } else if right {
                    Rectangle::new(
                        Point::from((
                            geometry.loc.x + geometry.size.w - along_right,
                            geometry.loc.y,
                        )),
                        Size::from((along_right, geometry.size.h)),
                    )
                } else {
                    return None;
                })
            })
            .collect()
    }
}

/// Where a surface asking `request` of `output` is put: the edges it is
/// anchored to keep it at the margin, both edges of an axis stretch it
/// between them, and one edge with a size of its own keeps that size there.
fn placement(
    request: &LayerSurfaceCachedState,
    output: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let anchor = request.anchor;
    let margin = request.margin;
    let side = |desired: i32, both: bool, margin: (i32, i32), total: i32| {
        if both {
            total
                .saturating_sub(margin.0)
                .saturating_sub(margin.1)
                .max(1)
        } else if desired > 0 {
            desired.min(total)
        } else {
            total
        }
    };
    let width = side(
        request.size.w,
        anchor.anchored_horizontally(),
        (margin.left, margin.right),
        output.size.w,
    );
    let height = side(
        request.size.h,
        anchor.anchored_vertically(),
        (margin.top, margin.bottom),
        output.size.h,
    );
    let x = if anchor.anchored_horizontally() || anchor.contains(Anchor::LEFT) {
        margin.left
    } else if anchor.contains(Anchor::RIGHT) {
        output.size.w - width - margin.right
    } else {
        (output.size.w - width) / 2
    };
    let y = if anchor.anchored_vertically() || anchor.contains(Anchor::TOP) {
        margin.top
    } else if anchor.contains(Anchor::BOTTOM) {
        output.size.h - height - margin.bottom
    } else {
        (output.size.h - height) / 2
    };
    Rectangle::new(
        Point::from((output.loc.x + x, output.loc.y + y)),
        Size::from((width, height)),
    )
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layers.shell
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        tracing::debug!(%namespace, ?layer, "layer surface");
        self.add_layer_surface(surface, output.as_ref(), layer);
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: LayerSurfaceConfigure) {
        self.layer_configured(&surface, &configure);
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let wl_surface = surface.wl_surface().clone();
        self.remove_layer_surface(&wl_surface);
    }
}

smithay::delegate_layer_shell!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        anchor: Anchor,
        size: (i32, i32),
        margin: (i32, i32, i32, i32),
    ) -> LayerSurfaceCachedState {
        LayerSurfaceCachedState {
            size: Size::from(size),
            anchor,
            margin: smithay::wayland::shell::wlr_layer::Margins {
                top: margin.0,
                right: margin.1,
                bottom: margin.2,
                left: margin.3,
            },
            ..LayerSurfaceCachedState::default()
        }
    }

    fn output() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((0, 0)), Size::from((1000, 800)))
    }

    #[test]
    fn a_bar_anchored_to_both_sides_spans_the_output_at_its_margins() {
        let placed = placement(
            &request(
                Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                (0, 40),
                (0, 0, 0, 0),
            ),
            output(),
        );
        assert_eq!(
            placed,
            Rectangle::new(Point::from((0, 0)), Size::from((1000, 40)))
        );
    }

    #[test]
    fn a_panel_hangs_from_the_edge_it_is_anchored_to_at_the_size_it_asked_for() {
        let placed = placement(
            &request(Anchor::TOP | Anchor::RIGHT, (380, 500), (34, 8, 0, 0)),
            output(),
        );
        assert_eq!(
            placed,
            Rectangle::new(Point::from((1000 - 380 - 8, 34)), Size::from((380, 500)))
        );
        let corner = placement(
            &request(Anchor::TOP | Anchor::LEFT, (300, 200), (0, 0, 0, 0)),
            output(),
        );
        assert_eq!(corner.loc, Point::from((0, 0)));
        // Anchored to the bottom, it keeps its bottom edge at the margin.
        let low = placement(
            &request(Anchor::BOTTOM | Anchor::RIGHT, (300, 200), (0, 10, 20, 0)),
            output(),
        );
        assert_eq!(low.loc, Point::from((1000 - 300 - 10, 800 - 200 - 20)));
    }

    #[test]
    fn a_surface_anchored_to_nothing_is_centred() {
        let placed = placement(
            &request(Anchor::empty(), (400, 300), (0, 0, 0, 0)),
            output(),
        );
        assert_eq!(placed.loc, Point::from((300, 250)));
    }

    #[test]
    fn a_stretched_axis_keeps_the_margins() {
        let placed = placement(
            &request(
                Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                (0, 32),
                (4, 4, 0, 4),
            ),
            output(),
        );
        assert_eq!(
            placed,
            Rectangle::new(Point::from((4, 4)), Size::from((1000 - 8, 32)))
        );
    }

    #[test]
    fn a_size_larger_than_its_output_is_kept_inside_it() {
        let placed = placement(
            &request(Anchor::TOP | Anchor::LEFT, (2000, 4000), (0, 0, 0, 0)),
            output(),
        );
        assert_eq!(placed.size, Size::from((1000, 800)));
    }
}
