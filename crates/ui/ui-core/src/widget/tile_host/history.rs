//! The History a tile hosts for its component: installing it, and the window bar's control for it.
use super::TileHost;
use crate::bound_dispatch::BoundWidget as _;
use automerge::AutoCommit;

impl TileHost {
    pub(super) fn install_history_registration(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) {
        let owner = self.history_owner();
        match crate::history_mode::host::make_history(registration, &AutoCommit::new()) {
            Ok(mut history) => {
                history.set_enclosing_owner(owner);
                history.set_keyboard_active(false);
                if let Some(tab_presents) = self.window_history {
                    history.remove_fallback_bar();
                    history.set_open_button(!tab_presents);
                    history.set_window_presents(tab_presents);
                }
                self.history = Some(history);
                self.attach_live(crate::control_domain::DomainAttachment::Dynamic);
                self.structure_dirty = true;
            }
            Err(error) => {
                self.resource_error = Some(format!("History unavailable: {error}"));
                self.structure_dirty = true;
            }
        }
    }

    #[must_use]
    pub fn history_control(&self) -> Option<crate::widget::HistoryControl> {
        match &self.history {
            Some(history) => history
                .offers_control()
                .then(|| crate::widget::HistoryControl::of(history.is_open())),
            None => self
                .app_history()
                .and_then(crate::widget::WindowHistory::window_history_control),
        }
    }

    pub fn invoke_history_control(
        &mut self,
        control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
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
}
