//! The player's controls as slots in the window's bar: the library's toggle as the
//! sidebar, the video's title as the location and Open as an action. Every slot runs the
//! player's published action, so a press, an assistive activation and an agent take one
//! path.

use ui::window_toolbar::{ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::PlayerState;
use crate::actions::VideoEffect;

impl PlayerState {
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        WindowToolbar {
            sidebar: Some(self.library_visible),
            location: self.player.has_media().then(|| ToolbarLocation {
                label: self.title.clone(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                self.bar
                    .entry(
                        "folder-open",
                        crate::i18n::t("video-open-video"),
                        VideoEffect::OpenVideo { path: None },
                    )
                    .enabled(self.portal_open.is_none()),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the bar, then declare the slots for what the player shows
    /// now.
    pub(super) fn run_window_toolbar(&self) {
        for event in ui::window_toolbar::take_events() {
            if event == ToolbarEvent::ToggleSidebar {
                self.request_library_visibility_change();
            } else if let Some(effect) = self.bar.pressed(&event) {
                self.request(&effect);
            }
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
