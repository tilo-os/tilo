//! Text input for clients (`text-input-v3`) served by a trusted input
//! method (`input-method-v2`, `virtual-keyboard-v1`): the on-screen
//! keyboard or an IME. Its candidate popups are drawn under the text they
//! belong to, above every window.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical coordinates of windows and outputs, far from i32 limits"
)]

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface};
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;

use crate::render::OutputRenderElement;
use crate::state::{ClientState, State};

fn trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(ClientState::trusted)
}

pub(crate) struct TextInput {
    popups: Vec<PopupSurface>,
    _text_inputs: TextInputManagerState,
    _input_method: InputMethodManagerState,
    _virtual_keyboard: VirtualKeyboardManagerState,
}

impl TextInput {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        Self {
            popups: Vec::new(),
            _text_inputs: TextInputManagerState::new::<State>(display),
            // Only the session's own input methods may read and type text.
            _input_method: InputMethodManagerState::new::<State, _>(display, trusted),
            _virtual_keyboard: VirtualKeyboardManagerState::new::<State, _>(display, trusted),
        }
    }
}

impl State {
    /// Input method popups on `output`, front first.
    pub(crate) fn input_popup_elements<R>(
        &self,
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
        let scale = Scale::from(output.current_scale().fractional_scale());
        self.text_input
            .popups
            .iter()
            .rev()
            .filter_map(|popup| {
                let parent = popup.get_parent()?;
                let text = popup.text_input_rectangle();
                // Under the text being composed.
                let at = parent.location.loc + Point::from((text.loc.x, text.loc.y + text.size.h))
                    - geometry.loc;
                let at: Point<i32, Physical> = at.to_physical_precise_round(scale);
                Some(render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    at,
                    scale,
                    1.0,
                    Kind::Unspecified,
                ))
            })
            .flatten()
            .collect()
    }
}

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.text_input
            .popups
            .retain(|popup| popup.wl_surface() != surface.wl_surface());
        self.text_input.popups.push(surface);
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        self.text_input
            .popups
            .retain(|popup| popup.wl_surface() != surface.wl_surface());
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        self.new_popup(surface);
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == parent)
            })
            .and_then(|window| self.space.element_geometry(window))
            .unwrap_or_default()
    }
}

smithay::delegate_text_input_manager!(State);
smithay::delegate_input_method_manager!(State);
smithay::delegate_virtual_keyboard_manager!(State);
