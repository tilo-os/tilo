//! Music's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the library's toggle, Back while there is somewhere to go
//! back to, the destination (or the album, artist or playlist) as the
//! window's location, and on Home the one action of the page: open an
//! audio file. Search is the bar's own.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{MusicApp, View};

pub(super) const BACK: &str = "back";
pub(super) const OPEN_FILE: &str = "open-file";

impl MusicApp {
    /// The window bar's slots for what the window shows now.
    fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar {
            sidebar: Some(self.drawer.is_open()),
            location: Some(ToolbarLocation {
                label: self.view_title(),
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        };
        if self.nav.can_go_back() {
            toolbar
                .leading
                .push(ToolbarAction::new(BACK, "arrow-left", "Back"));
        }
        if self.view == View::Home {
            toolbar.actions.push(ToolbarAction::new(
                OPEN_FILE,
                "folder-open",
                crate::i18n::t("music-open-audio-file"),
            ));
        }
        toolbar
    }

    /// Declare the window bar's slots.
    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run what the person did in the window's bar: the sidebar's toggle
    /// and Back here, Open audio file as the library's action, through the
    /// window's controls.
    pub(super) fn take_window_toolbar_events(&mut self) {
        for event in ui::window_toolbar::take_events() {
            match event {
                ToolbarEvent::ToggleSidebar => self.toggle_sidebar(),
                ToolbarEvent::Action { id } if id == BACK => self.back_requested.set(true),
                ToolbarEvent::Action { id } if id == OPEN_FILE => {
                    if let Some(invocation) = Self::open_file_invocation() {
                        self.bar_controls.push_back(invocation);
                    }
                }
                _ => continue,
            }
            self.dirty = true;
        }
    }
}
