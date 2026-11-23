//! The action palette a window asks the person in: a confirmation an ordinary action waits on.

use super::FrameHarness;
use crate::action_palette::ActionPaletteBox;
use crate::app_core::AppCore as _;

impl FrameHarness {
    /// A palette for this window.
    pub(super) fn new_action_palette(&self) -> ActionPaletteBox {
        crate::action_palette::make_action_palette(self.text.font_system.locale().to_owned())
    }

    /// Ask the person about `request` in the palette.
    pub(super) fn present_ordinary(
        &mut self,
        request: crate::bound_dispatch::OrdinaryConfirmation,
    ) {
        let mut palette = self.new_action_palette();
        self.palette_window = self.tree().invoking_window_rect();
        let anchor = self.tree().acting_control_rect();
        self.discard_layout_tree();
        self.focused = None;
        palette.anchor_confirmation(anchor);
        palette.activate_ordinary(request);
        let went_ahead = palette.went_ahead();
        self.action_palette = Some(palette);
        if went_ahead {
            self.finish_action_palette_input();
        }
    }
}
