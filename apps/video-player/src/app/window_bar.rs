//! The player's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the library's toggle as the sidebar, the open
//! video's title as the location and Open as an action. The bar's History
//! control is the window's; the player draws nothing over the picture for
//! any of them.
//!
//! While the video plays full screen the desktop's chrome, this bar with it,
//! steps aside and the player's own controls are all that shows.
//!
//! Every slot runs the player's published action for it
//! ([`crate::actions::VideoEffect`]) through the framework's door, so a press
//! in the bar, an assistive technology and an agent take one path.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::PlayerApp;

/// The bar action that opens a video.
pub(crate) const OPEN: &str = "video-player.open";

impl PlayerApp {
    /// The window bar's slots for what the player shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        WindowToolbar {
            sidebar: Some(self.library_visible),
            location: self.player.has_media().then(|| ToolbarLocation {
                label: self.title.clone(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new(OPEN, "folder-open", "Open video")
                    .enabled(self.portal_open.is_none()),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the player shows now.
    pub(super) fn run_window_toolbar(&self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar: the sidebar shows
    /// or hides the library and Open opens a video, as their published
    /// actions do.
    pub(crate) fn run_window_bar_event(&self, event: &ToolbarEvent) {
        match event {
            ToolbarEvent::ToggleSidebar => self.request_library_visibility_change(),
            ToolbarEvent::Action { id } if id == OPEN => self.request_open_action(),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
