//! The action palette whose code may still be on its way (`lazy-palettes`):
//! what a window asked of it meanwhile (a confirmation, search) waits here
//! and is shown once the code is in.

use super::FrameHarness;
use crate::action_palette::ActionPaletteBox;

/// What waits on the palette's code.
#[derive(Default)]
pub(super) struct Waiting {
    pub(super) ordinary: Option<crate::bound_dispatch::OrdinaryConfirmation>,
    pub(super) search: Option<crate::search::SearchRequest>,
}

impl FrameHarness {
    /// A palette for this window, or `None` while its code is still on its
    /// way (and then fetched).
    pub(super) fn new_action_palette(&self) -> Option<ActionPaletteBox> {
        crate::action_palette::make_action_palette(self.text.font_system.locale().to_owned())
    }

    /// Ask the person about `request` in the palette, or once its code is in.
    pub(super) fn present_ordinary(
        &mut self,
        request: crate::bound_dispatch::OrdinaryConfirmation,
    ) {
        let Some(mut palette) = self.new_action_palette() else {
            self.palette_waiting.ordinary = Some(request);
            return;
        };
        // Read before the tree it is found in is discarded.
        self.palette_window = self.invoking_window_rect();
        let anchor = self.acting_control_rect();
        self.discard_layout_tree();
        self.focused = None;
        // The question stands at the control the person used.
        palette.anchor_confirmation(anchor);
        palette.activate_ordinary(request);
        let went_ahead = palette.went_ahead();
        self.action_palette = Some(palette);
        if went_ahead {
            self.finish_action_palette_input();
        }
    }

    /// Show what waited on the palette's code, once it is in.
    /// Asking for the code here, as the window first lays out, fetches it
    /// ahead of the first prompt or search it shows.
    pub(super) fn adopt_loaded_palette(&mut self) {
        let ready = crate::action_palette::ACTION_PALETTE.get().is_some();
        let waiting = &self.palette_waiting;
        let waits = waiting.ordinary.is_some() || waiting.search.is_some();
        if !ready || !waits {
            return;
        }
        if let Some(request) = self.palette_waiting.ordinary.take() {
            self.present_ordinary(request);
        }
        if let Some(request) = self.palette_waiting.search.take()
            && let Err(error) = self.open_search(&request)
        {
            tracing::warn!(%error, "search did not open");
        }
    }
}
