//! The History a tile hosts for its component: installing it, the window bar's control for it, and
//! History whose code is still on its way (`lazy-history`): the control is there at once, a press
//! opens it once the code is in, and a quiet panel stands in its place meanwhile.
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::TileHost;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget as _;
use automerge::AutoCommit;

impl TileHost {
    pub(super) fn install_history_registration(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) {
        let owner = self.history_owner();
        match crate::history_mode::host::make_history(registration, &AutoCommit::new()) {
            Ok(crate::history_mode::host::MadeHistory::Loading(registration)) => {
                self.pending_history = Some(registration);
            }
            Ok(crate::history_mode::host::MadeHistory::Ready(mut history)) => {
                history.set_enclosing_owner(owner);
                history.set_keyboard_active(false);
                if let Some(tab_presents) = self.window_history {
                    history.remove_fallback_bar();
                    history.set_open_button(!tab_presents);
                    history.set_window_presents(tab_presents);
                }
                self.history = Some(history);
                self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
                self.structure_dirty = true;
            }
            Err(error) => {
                self.record_error = Some(format!("History unavailable: {error}"));
                self.structure_dirty = true;
            }
        }
    }

    pub(super) fn adopt_loaded_history(&mut self) -> bool {
        if self.pending_history.is_none() || !crate::history_mode::host::history_code_ready() {
            return false;
        }
        let Some(registration) = self.pending_history.take() else {
            return false;
        };
        self.install_history_registration(registration);
        if std::mem::take(&mut self.history_opening)
            && let Some(history) = &self.history
        {
            history.request_open();
        }
        true
    }

    #[must_use]
    pub fn history_control(&self) -> Option<crate::widget::HistoryControl> {
        match &self.history {
            Some(history) => history
                .offers_control()
                .then(|| crate::widget::HistoryControl::of(history.is_open())),
            None if self.pending_history.is_some() => {
                Some(crate::widget::HistoryControl::of(self.history_opening))
            }
            None => self
                .app_history()
                .and_then(crate::widget::WindowHistory::window_history_control),
        }
    }

    pub fn invoke_history_control(
        &mut self,
        control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        if self.history.is_none() && self.pending_history.is_some() {
            self.history_opening = control == crate::widget::HistoryControl::Closed;
            self.structure_dirty = true;
            return Ok(());
        }
        let Some(history) = self.history.as_deref_mut() else {
            return self
                .app_history_mut()
                .ok_or_else(|| "History is unavailable".to_owned())?
                .invoke_window_history_control(control);
        };
        crate::history_mode::host::press_window_control(history, control)?;
        self.structure_dirty = true;
        Ok(())
    }

    pub(super) fn draw_history_loading(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.history_opening || self.history.is_some() {
            return;
        }
        let width = crate::history_mode::SIDE_PANEL_WIDTH.min(self.rect.width());
        let panel = Rect::new(
            self.rect.x1 - width,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        crate::lazy_code::draw_loading(scene, transform, tr, theme, panel, "history-loading");
    }
}
