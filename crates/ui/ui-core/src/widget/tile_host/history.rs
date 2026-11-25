//! The History a tile hosts for its component: installing it, and the window bar's control for it.
use super::TileHost;
#[cfg(feature = "model")]
use crate::bound_dispatch::BoundWidget as _;
#[cfg(feature = "model")]
use automerge::AutoCommit;

impl TileHost {
    #[cfg(feature = "model")]
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
        #[cfg(feature = "model")]
        if let Some(history) = &self.history {
            return history
                .offers_control()
                .then(|| crate::widget::HistoryControl::of(history.is_open()));
        }
        self.app_history()
            .and_then(crate::widget::WindowHistory::window_history_control)
    }

    pub fn invoke_history_control(
        &mut self,
        control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        #[cfg(feature = "model")]
        if let Some(history) = self.history.as_deref_mut() {
            crate::history_mode::host::press_window_control(history, control)?;
            self.structure_dirty = true;
            return Ok(());
        }
        self.app_history_mut()
            .ok_or_else(|| "History is unavailable".to_owned())?
            .invoke_window_history_control(control)
    }
}

#[cfg(feature = "model")]
impl TileHost {
    /// Whether a History is around the app.
    pub(super) fn has_history(&self) -> bool {
        self.history.is_some()
    }

    /// Whether a change below this tile is carried out under the node it
    /// keeps: with no History around the app, or a History that is closed and
    /// lays the live app out in its body.
    pub(super) fn carries_children_in_place(&self) -> bool {
        self.history
            .as_ref()
            .is_none_or(|history| history.lays_out_in_place())
    }

    /// Whether the live app is drawn: always, unless a History around it
    /// shows a version in its place.
    pub(super) fn composes_live_app(&self) -> bool {
        self.history
            .as_ref()
            .is_none_or(|history| history.live_visible())
    }

    /// Whether the live app takes input: always, unless a History around it
    /// keeps it inert.
    pub(super) fn live_interactive(&self) -> bool {
        self.history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
    }
}

/// History is the workspace's record of what a window shows, so without
/// Tilo's model a tile hosts none: its app is always laid out in place, drawn
/// and interactive.
#[cfg(not(feature = "model"))]
#[allow(
    clippy::unused_self,
    reason = "the model build reads the tile's History"
)]
impl TileHost {
    pub(super) const fn has_history(&self) -> bool {
        false
    }

    pub(super) const fn carries_children_in_place(&self) -> bool {
        true
    }

    pub(super) const fn composes_live_app(&self) -> bool {
        true
    }

    pub(super) const fn live_interactive(&self) -> bool {
        true
    }
}
