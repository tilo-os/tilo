//! Code's controls, as slots in the window's bar ([`ui::window_toolbar`]).
//!

use ui::window_toolbar::{ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::CodeApp;

impl CodeApp {
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self
            .bar
            .as_ref()
            .map(ui::widget::WindowBarLink::slots)
            .unwrap_or_default();
        toolbar.location = self.session.active_buffer().map(|buffer| ToolbarLocation {
            label: buffer.name.clone(),
            edited: buffer.dirty,
            ..ToolbarLocation::default()
        });
        let panel = if self.panel_visible {
            crate::resource::CATALOG.t("code-editor-hide-panel")
        } else {
            crate::resource::CATALOG.t("code-editor-show-panel")
        };
        toolbar.actions.push(
            self.panel_entry
                .entry("panel-bottom", panel, ())
                .on(self.panel_visible)
                .enabled(self.live),
        );
        toolbar
    }

    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    pub(super) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        if self.bar.as_ref().is_some_and(|bar| bar.event(event)) {
            return;
        }
        if self.panel_entry.pressed(event).is_some() && self.live {
            self.toggle_bottom_panel();
        }
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
